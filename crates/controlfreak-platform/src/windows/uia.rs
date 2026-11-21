//! UI Automation queries and semantic actions scoped to one window.
//!
//! A query reads the UI Automation subtree of the window a target reference names and returns
//! its controls. It changes nothing: no pattern is invoked and no value is read. The element
//! references it issues are accepted as the root of a later query, and by `act_on_element`.
//!
//! An action (invoke, toggle, expand, collapse) is a desktop mutation. It runs under the input
//! lock after the session's target passed every input check and is the foreground window itself.
//! Its element reference must name the same target reference, window and process instance, and
//! the helper finds the element again by its runtime ID inside the window: it must be exactly one
//! element, of the window's process, enabled and supporting the action's pattern. Nothing falls
//! back to coordinates.
//!
//! Provider calls are cross-process COM calls into the target application, which can hang or
//! fault. They run in a helper process (`--uia-helper`, see `uia_process`), on a dedicated
//! multithreaded-apartment walker thread. Three bounds keep a hung provider from hanging the
//! server:
//!
//! 1. The walker checks the request's deadline before every provider call and stops with what it
//!    has read, reported as `partial` with limit `timeout`.
//! 2. When one provider call does not return, the helper's own watchdog answers at the deadline
//!    with what was read so far, or with `accessibility_unavailable` if not even the root
//!    answered, and then exits: a blocked COM call cannot be cancelled, only abandoned with its
//!    process. For an action whose pattern call was made, the answer says so, and the server
//!    reports the delivery as unknown.
//! 3. The server kills a helper that has not answered `HELPER_GRACE` after the deadline.
//!
//! A helper that was abandoned or killed says nothing about whether the target recovered.

use super::uia_process::{HelperKind, HelperPool, HelperReply, read_bounded_line};
// The command type is named for OCR, its first helper; it starts any helper mode.
use crate::OcrHelperCommand as HelperCommand;
use controlfreak_core::{
    DisplayInfo, ElementAction, ElementActionRequest, ElementBounds, ElementInfo, ElementPattern,
    ElementQueryCounts, ElementQueryLimit, ElementRole, FailureCause, FindElementsRequest,
    FindElementsResult, MutationControl, PlatformError,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::VecDeque,
    io::{BufReader, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// Elements one query reads at most, matching or not.
const NODE_LIMIT: u32 = 5_000;
/// Longest name returned, in characters. A longer one is cut and flagged.
const MAX_NAME_CHARS: usize = 256;
/// Size budget for the elements of one result, as the MCP client receives them.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// Serialized size an element reference and its field add to each element.
const ELEMENT_REF_BYTES: usize = 64;
/// How long after the request deadline the helper's watchdog waits for the walker before it
/// answers with what was read and abandons the walker.
const WATCHDOG_GRACE: Duration = Duration::from_millis(250);
/// How long after the request deadline the server waits for the helper before it kills it. It
/// covers starting the helper process and the UI Automation client.
const HELPER_GRACE: Duration = Duration::from_secs(3);
const HELPER_IDLE_EXIT: Duration = Duration::from_mins(1);
const MAX_REQUESTS_PER_HELPER: u32 = 200;
const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// An element reference can scope a later query for this long.
const ELEMENT_REF_TTL: Duration = Duration::from_mins(2);
const MAX_ELEMENT_REFS: usize = 4_096;
/// How long finding the element again and its pattern call may take. UI Automation's own
/// transaction timeout (2 s, see `uia_native`) ends most hung calls before it.
const ACTION_TIMEOUT_MS: u32 = 3_000;
const ACTION: &str = "act_on_element";

/// Starts the reason of a reply whose helper could not create its UI Automation client.
const CLIENT_START_FAILED: &str = "the UI Automation client could not start";

const UIA_KIND: HelperKind = HelperKind {
    label: "UI Automation",
    operation: "find_elements",
    stdout_thread: "controlfreak-uia-stdout",
    stderr_thread: "controlfreak-uia-stderr",
    spawn_stage: "uia_helper_spawn",
    wait_stage: "uia_helper_wait",
    reply_limit: MAX_REPLY_BYTES,
    max_requests: MAX_REQUESTS_PER_HELPER,
    error: unavailable,
    retry_unstarted: true,
};

static UIA_HELPER: HelperKind = UIA_KIND;

/// The same helper process, asked to act. A helper that exits without a reply may have acted, so
/// the request is never sent again.
static UIA_ACTION_HELPER: HelperKind = HelperKind {
    operation: ACTION,
    error: action_failed,
    retry_unstarted: false,
    ..UIA_KIND
};

fn unavailable(reason: String) -> PlatformError {
    PlatformError::failed(
        FailureCause::AccessibilityUnavailable,
        "find_elements",
        reason,
    )
}

fn action_failed(reason: String) -> PlatformError {
    PlatformError::failed(FailureCause::AccessibilityUnavailable, ACTION, reason)
}

/// What one walk reads: a window (and optionally one element inside it), the processes whose
/// elements count as part of it, the filters and the limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct WalkQuery {
    pub hwnd: usize,
    pub pid: u32,
    /// The target's process, plus the content process of a packaged app's frame.
    pub allowed_pids: Vec<u32>,
    /// Runtime ID of the element whose subtree is read instead of the whole window.
    pub within: Option<Vec<i32>>,
    pub name: Option<String>,
    pub role: Option<ElementRole>,
    pub max_depth: u32,
    pub max_elements: u32,
    pub timeout_ms: u32,
}

/// One semantic action: the element with `runtime_id` inside the window, and what to do with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ActQuery {
    pub hwnd: usize,
    /// The window's own process. Unlike a query, an action never reaches the content process of
    /// a packaged app's frame: its process instance cannot be checked against the reference.
    pub pid: u32,
    pub runtime_id: Vec<i32>,
    pub action: ElementAction,
    pub timeout_ms: u32,
}

/// Why an action failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ActError {
    /// The element has no such pattern: nothing was done.
    Refused(String),
    /// The last check before the call refused it (cancelled, or the window is no longer in
    /// front): nothing was done.
    NotReady(PlatformError),
    /// The pattern call failed, a provider timeout included: the action may have happened.
    Unknown(String),
}

/// What the walk needs to know about one element, read in one cached batch.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent UI Automation properties of one element, reported as they are"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct NodeFacts {
    pub runtime_id: Vec<i32>,
    pub control_type: i32,
    pub name: String,
    pub enabled: bool,
    pub focusable: bool,
    pub focused: bool,
    pub offscreen: bool,
    pub password: bool,
    pub process_id: u32,
    /// Left, top, width and height in virtual-screen physical pixels; `None` when empty.
    pub rect: Option<[i32; 4]>,
    /// One bit per entry of `ElementPattern::ALL`.
    pub patterns: u32,
}

/// Why a provider call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TreeError {
    /// The element no longer exists.
    Gone(String),
    /// Any other failure, including a provider timeout.
    Failed(String),
}

impl std::fmt::Display for TreeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone(reason) | Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

/// A UI Automation client, or a fake of one. The walk asks for the root and then only for first
/// children and next siblings, so it can never reach outside the root's subtree.
pub(super) trait Tree {
    type Node;
    /// The window's element, or the element with runtime ID `within` in the window's subtree.
    fn root(
        &self,
        hwnd: usize,
        within: Option<&[i32]>,
    ) -> Result<(Self::Node, NodeFacts), TreeError>;
    fn first_child(&self, node: &Self::Node) -> Result<Option<(Self::Node, NodeFacts)>, TreeError>;
    fn next_sibling(&self, node: &Self::Node)
    -> Result<Option<(Self::Node, NodeFacts)>, TreeError>;
    /// Every element with runtime ID `runtime_id` in the window's subtree, the window included.
    fn find_all(
        &self,
        hwnd: usize,
        runtime_id: &[i32],
    ) -> Result<Vec<(Self::Node, NodeFacts)>, TreeError>;
    /// Performs `action` through the element's control pattern. `ready` runs after the pattern
    /// was obtained and immediately before the pattern call, which is made only if it passes.
    fn act(
        &self,
        node: &Self::Node,
        action: ElementAction,
        ready: &dyn Fn() -> Result<(), PlatformError>,
    ) -> Result<(), ActError>;
    /// The last desktop check before a pattern call: window `hwnd` of process `pid` is the
    /// foreground window, not minimized, on the unlocked Default input desktop.
    fn in_front(&self, hwnd: usize, pid: u32) -> Result<(), PlatformError>;
}

/// One element as the helper reports it, before the server maps its rectangle and issues its
/// reference.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent UI Automation properties of one element, reported as they are"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RawElement {
    pub runtime_id: Vec<i32>,
    pub role: ElementRole,
    pub name: String,
    pub name_truncated: bool,
    pub password: bool,
    pub depth: u32,
    pub enabled: bool,
    pub focusable: bool,
    pub focused: bool,
    pub offscreen: bool,
    pub rect: Option<[i32; 4]>,
    pub patterns: Vec<ElementPattern>,
}

/// What a walk read. The helper's watchdog reads it while the walk runs, so a hung provider
/// still yields what was read before it hung.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub root_answered: bool,
    pub elements: Vec<RawElement>,
    pub limits_reached: Vec<ElementQueryLimit>,
    pub provider_errors: u32,
    pub skipped_other_process: u32,
    pub visited: u32,
    pub bytes: usize,
}

impl Snapshot {
    fn limit(&mut self, limit: ElementQueryLimit) {
        if !self.limits_reached.contains(&limit) {
            self.limits_reached.push(limit);
        }
    }
}

/// Maps a UI Automation control type ID to a role.
fn role_of(control_type: i32) -> ElementRole {
    use ElementRole as R;
    match control_type {
        50000 => R::Button,
        50001 => R::Calendar,
        50002 => R::CheckBox,
        50003 => R::ComboBox,
        50004 => R::Edit,
        50005 => R::Hyperlink,
        50006 => R::Image,
        50007 => R::ListItem,
        50008 => R::List,
        50009 => R::Menu,
        50010 => R::MenuBar,
        50011 => R::MenuItem,
        50012 => R::ProgressBar,
        50013 => R::RadioButton,
        50014 => R::ScrollBar,
        50015 => R::Slider,
        50016 => R::Spinner,
        50017 => R::StatusBar,
        50018 => R::Tab,
        50019 => R::TabItem,
        50020 => R::Text,
        50021 => R::ToolBar,
        50022 => R::ToolTip,
        50023 => R::Tree,
        50024 => R::TreeItem,
        50025 => R::Custom,
        50026 => R::Group,
        50027 => R::Thumb,
        50028 => R::DataGrid,
        50029 => R::DataItem,
        50030 => R::Document,
        50031 => R::SplitButton,
        50032 => R::Window,
        50033 => R::Pane,
        50034 => R::Header,
        50035 => R::HeaderItem,
        50036 => R::Table,
        50037 => R::TitleBar,
        50038 => R::Separator,
        50039 => R::SemanticZoom,
        50040 => R::AppBar,
        _ => R::Unknown,
    }
}

fn element(facts: &NodeFacts, depth: u32) -> RawElement {
    // A password field's name is withheld too: some providers put the typed text in it.
    let (name, name_truncated) = if facts.password {
        (String::new(), false)
    } else {
        let mut chars = facts.name.chars();
        let name: String = chars.by_ref().take(MAX_NAME_CHARS).collect();
        (name, chars.next().is_some())
    };
    RawElement {
        runtime_id: facts.runtime_id.clone(),
        role: role_of(facts.control_type),
        name,
        name_truncated,
        password: facts.password,
        depth,
        enabled: facts.enabled,
        focusable: facts.focusable,
        focused: facts.focused,
        offscreen: facts.offscreen,
        rect: facts.rect,
        patterns: ElementPattern::ALL
            .iter()
            .enumerate()
            .filter(|(bit, _)| facts.patterns & (1 << bit) != 0)
            .map(|(_, pattern)| *pattern)
            .collect(),
    }
}

fn matches(query: &WalkQuery, facts: &NodeFacts) -> bool {
    if query
        .role
        .is_some_and(|role| role_of(facts.control_type) != role)
    {
        return false;
    }
    match &query.name {
        // A password field's name is never compared, so a filter cannot probe it either.
        Some(_) if facts.password => false,
        Some(name) => facts.name.to_lowercase().contains(&name.to_lowercase()),
        None => true,
    }
}

fn lock(snapshot: &Mutex<Snapshot>) -> std::sync::MutexGuard<'_, Snapshot> {
    snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Walks the subtree of the query's root in document order and records matching elements in
/// `out` as it goes.
///
/// # Errors
///
/// Only the root can fail the walk: `accessibility_unavailable` when its provider fails, and
/// `invalid_argument` for `within` when that element is no longer in the window. Every later
/// failure is counted in `provider_errors` and leaves the rest of the tree readable.
pub(super) fn walk<T: Tree>(
    tree: &T,
    query: &WalkQuery,
    deadline: Instant,
    out: &Mutex<Snapshot>,
) -> Result<(), PlatformError> {
    let (root, facts) = match tree.root(query.hwnd, query.within.as_deref()) {
        Ok(root) => root,
        Err(TreeError::Gone(_)) if query.within.is_some() => {
            return Err(stale_within("the element is no longer in the window"));
        }
        Err(error) => {
            return Err(unavailable(format!(
                "the window's UI Automation provider did not answer: {error}"
            )));
        }
    };
    if !query.allowed_pids.contains(&facts.process_id) {
        return Err(if query.within.is_some() {
            stale_within("the element no longer belongs to the window's process")
        } else {
            PlatformError::TargetInvalidated {
                reason: "the window no longer belongs to the approved process".to_owned(),
            }
        });
    }
    lock(out).root_answered = true;
    // Each entry is an element still to visit. The root's siblings are never asked for, so the
    // walk cannot leave the root's subtree.
    let mut pending = vec![(root, facts, 0_u32)];
    let mut returned = 0_u32;
    while let Some((node, facts, depth)) = pending.pop() {
        if Instant::now() >= deadline {
            lock(out).limit(ElementQueryLimit::Timeout);
            break;
        }
        {
            let mut snapshot = lock(out);
            if snapshot.visited >= NODE_LIMIT {
                snapshot.limit(ElementQueryLimit::NodeLimit);
                break;
            }
            snapshot.visited += 1;
        }
        if depth > 0 {
            match tree.next_sibling(&node) {
                Ok(Some((sibling, facts))) => pending.push((sibling, facts, depth)),
                Ok(None) => {}
                Err(_) => lock(out).provider_errors += 1,
            }
        }
        if !query.allowed_pids.contains(&facts.process_id) {
            // Another process's window inside this one: neither it nor its subtree is read.
            lock(out).skipped_other_process += 1;
            continue;
        }
        if matches(query, &facts) {
            if returned == query.max_elements {
                lock(out).limit(ElementQueryLimit::MaxElements);
                break;
            }
            let element = element(&facts, depth);
            let size = serde_json::to_vec(&element).map_or(MAX_RESPONSE_BYTES, |bytes| bytes.len())
                + ELEMENT_REF_BYTES;
            let mut snapshot = lock(out);
            if snapshot.bytes + size > MAX_RESPONSE_BYTES {
                snapshot.limit(ElementQueryLimit::ResponseBytes);
                break;
            }
            snapshot.bytes += size;
            snapshot.elements.push(element);
            returned += 1;
        }
        if Instant::now() >= deadline {
            lock(out).limit(ElementQueryLimit::Timeout);
            break;
        }
        match tree.first_child(&node) {
            Ok(Some(_)) if depth >= query.max_depth => lock(out).limit(ElementQueryLimit::MaxDepth),
            Ok(Some((child, facts))) => pending.push((child, facts, depth + 1)),
            Ok(None) => {}
            Err(_) => lock(out).provider_errors += 1,
        }
    }
    Ok(())
}

fn stale_within(reason: &str) -> PlatformError {
    PlatformError::InvalidArgument {
        argument: "within".to_owned(),
        reason: format!("{reason}; call find_elements again without it"),
    }
}

fn stale_element(reason: &str) -> PlatformError {
    PlatformError::InvalidArgument {
        argument: "element_ref".to_owned(),
        reason: format!("{reason}; call find_elements again for a new element_ref"),
    }
}

/// Finds the action's element again and performs the action. The element must be exactly one
/// element of the window's subtree with the reference's runtime ID, belong to the window's own
/// process, be enabled and support the action's pattern; otherwise nothing is done. Once the
/// pattern is obtained, and immediately before its call, the request must still be open and the
/// window still in front (`Tree::in_front`): the server checked both before the request, but
/// finding the element can take seconds. `dispatched` is set only for the call itself, so a hung
/// or failed call leaves it set: the action may have happened.
///
/// # Errors
///
/// `invalid_argument` for a stale, ambiguous, foreign, disabled or unsupported element,
/// `target_invalidated` when the window is gone or no longer in front, `cancelled` when the
/// request was closed, and `accessibility_unavailable` when the provider failed.
pub(super) fn perform<T: Tree>(
    tree: &T,
    query: &ActQuery,
    closed: &AtomicBool,
    dispatched: &AtomicBool,
) -> Result<(), PlatformError> {
    let found = match tree.find_all(query.hwnd, &query.runtime_id) {
        Ok(found) => found,
        Err(TreeError::Gone(_)) => {
            return Err(PlatformError::TargetInvalidated {
                reason: "the window no longer exists".to_owned(),
            });
        }
        Err(TreeError::Failed(reason)) => {
            return Err(action_failed(format!(
                "the window's UI Automation provider did not answer, so nothing was done: {reason}"
            )));
        }
    };
    let (node, facts) = match found.as_slice() {
        [] => return Err(stale_element("the element is no longer in the window")),
        [one] => one,
        _ => {
            return Err(stale_element(
                "more than one element of the window has this element's runtime ID",
            ));
        }
    };
    if facts.process_id != query.pid {
        return Err(stale_element(
            "the element does not belong to the window's process",
        ));
    }
    if !facts.enabled {
        return Err(stale_element("the element is disabled"));
    }
    let pattern = query.action.pattern();
    // The pattern's bit in `NodeFacts::patterns`, its index in `ElementPattern::ALL`.
    let bit = ElementPattern::ALL
        .iter()
        .position(|candidate| *candidate == pattern)
        .map_or(0, |index| 1_u32 << index);
    if facts.patterns & bit == 0 {
        return Err(PlatformError::InvalidArgument {
            argument: "action".to_owned(),
            reason: format!(
                "the element does not support the {pattern:?} pattern that this action needs; see its patterns in find_elements"
            ),
        });
    }
    let ready = || {
        // Set before `closed` is read, while the watchdog sets `closed` before it reads
        // `dispatched`: either this side sees the request closed and does not act, or the
        // watchdog's answer says the action may have happened.
        dispatched.store(true, Ordering::SeqCst);
        let checked = if closed.load(Ordering::SeqCst) {
            Err(PlatformError::failed(
                FailureCause::Cancelled,
                ACTION,
                "the request was cancelled before the action",
            ))
        } else {
            tree.in_front(query.hwnd, query.pid)
        };
        if checked.is_err() {
            dispatched.store(false, Ordering::SeqCst);
        }
        checked
    };
    match tree.act(node, query.action, &ready) {
        Ok(()) => Ok(()),
        Err(ActError::Refused(reason)) => Err(PlatformError::InvalidArgument {
            argument: "action".to_owned(),
            reason: format!("the element does not offer the pattern this action needs: {reason}"),
        }),
        Err(ActError::NotReady(error)) => Err(error),
        Err(ActError::Unknown(reason)) => Err(action_failed(format!(
            "the pattern call failed, so the action may or may not have happened; observe before acting again: {reason}"
        ))),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct UiaHelperRequest {
    id: u64,
    #[serde(flatten)]
    job: HelperJob,
}

/// What one request asks the helper to do.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
enum HelperJob {
    Walk(WalkQuery),
    Act(ActQuery),
}

impl HelperJob {
    const fn timeout_ms(&self) -> u32 {
        match self {
            Self::Walk(query) => query.timeout_ms,
            Self::Act(query) => query.timeout_ms,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum UiaHelperResponse {
    Success {
        snapshot: Snapshot,
    },
    /// The action's pattern call returned success.
    Acted,
    Error {
        error: PlatformError,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct UiaHelperReply {
    id: u64,
    #[serde(flatten)]
    response: UiaHelperResponse,
    /// The helper's UI Automation client was created. Only such a reply shows that UI
    /// Automation works here, whatever the window's provider answered.
    #[serde(default)]
    ready: bool,
    /// The helper abandoned a walker blocked in a provider call, or its client could not start,
    /// and exits after this reply.
    #[serde(default)]
    retire: bool,
    /// An action's pattern call was made and did not return a refusal, so an error reply leaves
    /// the action's effect unknown.
    #[serde(default)]
    dispatched: bool,
}

impl HelperReply for UiaHelperReply {
    fn id(&self) -> u64 {
        self.id
    }

    fn keeps_helper(&self) -> bool {
        !self.retire
    }
}

/// One request for the walker thread.
struct WalkJob {
    work: Work,
    done: mpsc::Sender<Result<(), PlatformError>>,
}

enum Work {
    Walk {
        query: WalkQuery,
        deadline: Instant,
        out: Arc<Mutex<Snapshot>>,
    },
    Act {
        query: ActQuery,
        closed: Arc<AtomicBool>,
        dispatched: Arc<AtomicBool>,
    },
}

/// Whether the walker's UI Automation client exists yet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClientState {
    Starting = 0,
    Ready = 1,
    Failed = 2,
}

/// Starts the walker thread, which creates the provider client with `open` and then serves
/// walk and action jobs in order. A client that cannot be created fails every job. `state` tells
/// the helper whether the client exists, even while the walker is blocked.
fn start_walker<T, F>(open: F, state: Arc<AtomicU8>) -> std::io::Result<mpsc::Sender<WalkJob>>
where
    T: Tree,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let (jobs, receiver) = mpsc::channel::<WalkJob>();
    thread::Builder::new()
        .name("controlfreak-uia-walker".to_owned())
        .spawn(move || {
            let tree = open();
            let ready = if tree.is_ok() {
                ClientState::Ready
            } else {
                ClientState::Failed
            };
            state.store(ready as u8, Ordering::Release);
            while let Ok(job) = receiver.recv() {
                let result = match (&tree, &job.work) {
                    (
                        Ok(tree),
                        Work::Walk {
                            query,
                            deadline,
                            out,
                        },
                    ) => walk(tree, query, *deadline, out),
                    (
                        Ok(tree),
                        Work::Act {
                            query,
                            closed,
                            dispatched,
                        },
                    ) => perform(tree, query, closed, dispatched),
                    (Err(reason), Work::Walk { .. }) => {
                        Err(unavailable(format!("{CLIENT_START_FAILED}: {reason}")))
                    }
                    (Err(reason), Work::Act { .. }) => Err(action_failed(format!(
                        "{CLIENT_START_FAILED}, so nothing was done: {reason}"
                    ))),
                };
                let _ = job.done.send(result);
            }
        })?;
    Ok(jobs)
}

pub(crate) fn serve_uia_helper(
    input: impl Read + Send + 'static,
    output: impl Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_uia_helper_with(
        input,
        output,
        super::uia_native::UiaClient::open,
        HELPER_IDLE_EXIT,
    )
}

/// Serves request lines until input ends, the helper stays idle for `idle_exit`, or a walker is
/// abandoned in a provider call that did not return. Closing input ends the helper at once: a
/// walk only reads, so there is nothing to finish, and an action that has not reached its pattern
/// call is not made.
fn serve_uia_helper_with<T, F>(
    input: impl Read + Send + 'static,
    mut output: impl Write,
    open: F,
    idle_exit: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    T: Tree,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let closed = Arc::new(AtomicBool::new(false));
    let (sender, requests) = mpsc::channel();
    {
        let closed = Arc::clone(&closed);
        thread::Builder::new()
            .name("controlfreak-uia-input".to_owned())
            .spawn(move || {
                let mut input = BufReader::new(input);
                while let Ok(Some(line)) = read_bounded_line(&mut input, MAX_REQUEST_BYTES) {
                    if sender.send(line).is_err() {
                        return;
                    }
                }
                closed.store(true, Ordering::Release);
            })?;
    }
    let client = Arc::new(AtomicU8::new(ClientState::Starting as u8));
    let walker = start_walker(open, Arc::clone(&client))?;
    while let Ok(line) = requests.recv_timeout(idle_exit) {
        let request: UiaHelperRequest = serde_json::from_slice(&line)?;
        let acting = matches!(request.job, HelperJob::Act(_));
        let timeout_ms = request.job.timeout_ms();
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        let out = Arc::new(Mutex::new(Snapshot::default()));
        let dispatched = Arc::new(AtomicBool::new(false));
        let (done, finished) = mpsc::channel();
        let work = match request.job {
            HelperJob::Walk(query) => Work::Walk {
                query,
                deadline,
                out: Arc::clone(&out),
            },
            HelperJob::Act(query) => Work::Act {
                query,
                closed: Arc::clone(&closed),
                dispatched: Arc::clone(&dispatched),
            },
        };
        walker.send(WalkJob { work, done })?;
        let mut retire = false;
        let result = loop {
            match finished.recv_timeout(Duration::from_millis(10)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(unavailable("the UI Automation walker stopped".to_owned()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if closed.load(Ordering::Acquire) {
                return Ok(());
            }
            if Instant::now() >= deadline + WATCHDOG_GRACE {
                // The walker is blocked in a provider call. Answer with what it read and exit:
                // the call cannot be cancelled, and a blocked walker cannot serve again.
                retire = true;
                if acting {
                    // A walker still finding the element must not act after this answer; see
                    // `perform` for the order of the two flags.
                    closed.store(true, Ordering::SeqCst);
                    break Err(action_failed(format!(
                        "the window's UI Automation provider did not answer within {timeout_ms} ms; the action may or may not have happened, so observe before acting again"
                    )));
                }
                let mut snapshot = lock(&out);
                if !snapshot.root_answered {
                    break Err(unavailable(format!(
                        "the window's UI Automation provider did not answer within {timeout_ms} ms"
                    )));
                }
                snapshot.limit(ElementQueryLimit::Timeout);
                snapshot.provider_errors += 1;
                break Ok(());
            }
        };
        let dispatched = dispatched.load(Ordering::SeqCst);
        // A pattern call that failed, a UI Automation timeout included, may have left the
        // provider blocked or the client in a bad state: this helper serves no further request.
        retire |= acting && dispatched && result.is_err();
        let response = match result {
            Ok(()) if acting => UiaHelperResponse::Acted,
            Ok(()) => UiaHelperResponse::Success {
                snapshot: lock(&out).clone(),
            },
            Err(error) => UiaHelperResponse::Error { error },
        };
        let client = client.load(Ordering::Acquire);
        // A client that never started (or is still starting behind a blocked walker) cannot
        // serve another request.
        retire |= client != ClientState::Ready as u8;
        serde_json::to_writer(
            &mut output,
            &UiaHelperReply {
                id: request.id,
                response,
                ready: client == ClientState::Ready as u8,
                retire,
                dispatched,
            },
        )?;
        output.write_all(b"\n")?;
        output.flush()?;
        if retire || closed.load(Ordering::Acquire) {
            break;
        }
    }
    Ok(())
}

/// The live window a target reference names, and the processes whose elements count as part of
/// it. Two scopes are equal only for the same window of the same process instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    pub hwnd: usize,
    pub pid: u32,
    /// Process creation time, so a reused process ID is a different scope.
    pub created: u64,
    pub allowed_pids: Vec<u32>,
}

/// What a query needs from the desktop and the helper; the backend supplies the real ones.
pub(super) trait QueryEnvironment {
    fn scope(&self, target_ref: &str) -> Result<Scope, PlatformError>;
    fn displays(&self) -> Result<Vec<DisplayInfo>, PlatformError>;
    fn walk(&self, query: &WalkQuery, control: &MutationControl)
    -> Result<Snapshot, PlatformError>;
}

/// The production environment: target references, displays and the warm helper.
pub(super) struct HelperEnvironment<'a> {
    pub command: &'a HelperCommand,
    pub pool: &'a HelperPool,
    /// Told whether the helper and its UI Automation client answered, for capability reporting.
    pub answered: &'a dyn Fn(Result<(), String>),
}

impl QueryEnvironment for HelperEnvironment<'_> {
    fn scope(&self, target_ref: &str) -> Result<Scope, PlatformError> {
        super::target::observation_scope(target_ref)
    }

    fn displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        super::desktop::enumerate_displays()
    }

    fn walk(
        &self,
        query: &WalkQuery,
        control: &MutationControl,
    ) -> Result<Snapshot, PlatformError> {
        let timeout = Duration::from_millis(u64::from(query.timeout_ms)) + HELPER_GRACE;
        let reply = self.pool.request::<UiaHelperReply>(
            &UIA_HELPER,
            self.command,
            control,
            timeout,
            |id| {
                serde_json::to_vec(&UiaHelperRequest {
                    id,
                    job: HelperJob::Walk(query.clone()),
                })
                .map_err(|error| unavailable(format!("could not encode the query: {error}")))
            },
            &|| {},
        );
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                if !control.is_cancelled() {
                    (self.answered)(Err(error.to_string()));
                }
                return Err(error);
            }
        };
        self.record(&reply);
        match reply.response {
            UiaHelperResponse::Success { snapshot } => Ok(snapshot),
            UiaHelperResponse::Error { error } => Err(error),
            UiaHelperResponse::Acted => Err(unavailable(
                "the UI Automation helper answered a query as an action".to_owned(),
            )),
        }
    }
}

impl HelperEnvironment<'_> {
    /// Tells capability reporting whether the helper's UI Automation client answered.
    fn record(&self, reply: &UiaHelperReply) {
        (self.answered)(match &reply.response {
            _ if reply.ready => Ok(()),
            UiaHelperResponse::Error { error } => Err(error.to_string()),
            UiaHelperResponse::Success { .. } | UiaHelperResponse::Acted => {
                Err("the UI Automation client did not start".to_owned())
            }
        });
    }
}

/// A failed action, and whether its pattern call may have run.
#[derive(Debug)]
pub(super) struct ActFailure {
    pub error: PlatformError,
    /// The helper may have acted: its pattern call was made, or the request reached a helper that
    /// gave no answer.
    pub dispatched: bool,
}

/// What a semantic action needs from the desktop and the helper; the backend supplies the real
/// ones.
pub(super) trait ActionEnvironment {
    /// Checks the session's target for this action: every check of pointer input except the
    /// hit test, and that the target window itself is the foreground window and not minimized.
    fn validate(&self, control: &MutationControl) -> Result<(), PlatformError>;
    fn scope(&self, target_ref: &str) -> Result<Scope, PlatformError>;
    /// Sends the action to the helper. `sending` runs immediately before the request is written.
    fn act(
        &self,
        query: &ActQuery,
        control: &MutationControl,
        sending: &dyn Fn(),
    ) -> Result<(), ActFailure>;
}

/// The production environment for actions: the session's target checks and the warm helper.
pub(super) struct ActionHelper<'a> {
    pub helper: HelperEnvironment<'a>,
}

impl ActionEnvironment for ActionHelper<'_> {
    fn validate(&self, control: &MutationControl) -> Result<(), PlatformError> {
        super::target::validate(control, ACTION, None)?;
        // A held modifier reaches an application that reads the keyboard state when it handles
        // the action, as it would reach a click.
        super::input::ensure_no_modifier_held(ACTION)?;
        super::target::ensure_target_in_front(control)
    }

    fn scope(&self, target_ref: &str) -> Result<Scope, PlatformError> {
        super::target::observation_scope(target_ref)
    }

    fn act(
        &self,
        query: &ActQuery,
        control: &MutationControl,
        sending: &dyn Fn(),
    ) -> Result<(), ActFailure> {
        let timeout = Duration::from_millis(u64::from(query.timeout_ms)) + HELPER_GRACE;
        let reply = self
            .helper
            .pool
            .request::<UiaHelperReply>(
                &UIA_ACTION_HELPER,
                self.helper.command,
                control,
                timeout,
                |id| {
                    serde_json::to_vec(&UiaHelperRequest {
                        id,
                        job: HelperJob::Act(query.clone()),
                    })
                    .map_err(|error| action_failed(format!("could not encode the action: {error}")))
                },
                sending,
            )
            .map_err(|error| {
                if !control.is_cancelled() {
                    (self.helper.answered)(Err(error.to_string()));
                }
                // Whether the request reached the helper is told by `sending`; once it did, a
                // helper that gave no answer may have acted.
                ActFailure {
                    error,
                    dispatched: true,
                }
            })?;
        self.helper.record(&reply);
        match reply.response {
            UiaHelperResponse::Acted => Ok(()),
            UiaHelperResponse::Error { error } => Err(ActFailure {
                error,
                dispatched: reply.dispatched,
            }),
            UiaHelperResponse::Success { .. } => Err(ActFailure {
                error: action_failed(
                    "the UI Automation helper answered an action as a query".to_owned(),
                ),
                dispatched: true,
            }),
        }
    }
}

/// Runs one semantic action on the session's target, after the checks above. The caller holds
/// the input lock. Delivery is recorded as unknown from the moment the request is written to the
/// helper, rejected when the helper says it did not act, and accepted when the pattern call
/// returned success.
///
/// # Errors
///
/// `target_invalidated` without a session target or for another window, every refusal of the
/// target checks, `invalid_argument` for a stale, ambiguous, foreign or unsuitable element, and
/// `accessibility_unavailable` when the provider failed or did not answer.
pub(super) fn act_on_element(
    request: &ElementActionRequest,
    control: &MutationControl,
    environment: &impl ActionEnvironment,
    refs: &ElementRefs,
) -> Result<(), PlatformError> {
    control.check(ACTION)?;
    request.validate()?;
    let approved = control
        .approved_target()
        .ok_or_else(|| PlatformError::TargetInvalidated {
            reason: "an approved target reference is required".to_owned(),
        })?;
    if approved != request.target_ref {
        return Err(PlatformError::TargetInvalidated {
            reason: "end the control session before acting on another window".to_owned(),
        });
    }
    environment.validate(control)?;
    let scope = environment.scope(&approved)?;
    let runtime_id = refs.resolve(&request.element_ref, &approved, &scope, "element_ref")?;
    control.check(ACTION)?;
    let query = ActQuery {
        hwnd: scope.hwnd,
        pid: scope.pid,
        runtime_id,
        action: request.action,
        timeout_ms: ACTION_TIMEOUT_MS,
    };
    let previous = Cell::new(None);
    match environment.act(&query, control, &|| {
        previous.set(Some(control.dispatch_started()));
    }) {
        Ok(()) => {
            control.dispatch_accepted(0);
            Ok(())
        }
        Err(failure) => {
            if !failure.dispatched
                && let Some(previous) = previous.get()
            {
                control.dispatch_rejected(previous);
            }
            Err(failure.error)
        }
    }
}

struct ElementEntry {
    id: String,
    target_ref: String,
    hwnd: usize,
    pid: u32,
    created: u64,
    runtime_id: Vec<i32>,
    issued: Instant,
}

/// Element references issued by recent queries. A reference names one element of one window of
/// one process instance, through its UI Automation runtime ID, for `ELEMENT_REF_TTL`.
#[derive(Default)]
pub(super) struct ElementRefs {
    entries: Mutex<VecDeque<ElementEntry>>,
    next: AtomicU64,
}

impl ElementRefs {
    /// The runtime ID that `id` names, if it was issued for `target_ref` in the same window and
    /// process instance as `scope` and has not expired. A refusal names `argument`, the field
    /// that carried the reference (`within` or `element_ref`).
    fn resolve(
        &self,
        id: &str,
        target_ref: &str,
        scope: &Scope,
        argument: &str,
    ) -> Result<Vec<i32>, PlatformError> {
        let stale = |reason: &str| {
            if argument == "within" {
                stale_within(reason)
            } else {
                stale_element(reason)
            }
        };
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries
            .iter()
            .find(|entry| entry.id == id && entry.issued.elapsed() < ELEMENT_REF_TTL)
            .ok_or_else(|| stale("unknown or expired element reference"))?;
        if entry.target_ref != target_ref {
            return Err(stale("the element reference belongs to another target_ref"));
        }
        if entry.pid != scope.pid || entry.created != scope.created {
            return Err(stale("the window's process changed"));
        }
        if entry.hwnd != scope.hwnd {
            return Err(stale("the window changed"));
        }
        Ok(entry.runtime_id.clone())
    }

    fn issue(&self, target_ref: &str, scope: &Scope, runtime_id: &[i32]) -> String {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|entry| entry.issued.elapsed() < ELEMENT_REF_TTL);
        let now = Instant::now();
        if let Some(entry) = entries.iter_mut().find(|entry| {
            entry.target_ref == target_ref
                && entry.hwnd == scope.hwnd
                && entry.pid == scope.pid
                && entry.created == scope.created
                && entry.runtime_id == runtime_id
        }) {
            entry.issued = now;
            return entry.id.clone();
        }
        while entries.len() >= MAX_ELEMENT_REFS {
            entries.pop_front();
        }
        let number = self.next.fetch_add(1, Ordering::Relaxed);
        let id = format!(
            "element-{:08x}-{number:x}",
            super::capture::baseline_process_nonce() & 0xffff_ffff
        );
        entries.push_back(ElementEntry {
            id: id.clone(),
            target_ref: target_ref.to_owned(),
            hwnd: scope.hwnd,
            pid: scope.pid,
            created: scope.created,
            runtime_id: runtime_id.to_vec(),
            issued: now,
        });
        id
    }
}

/// Maps a virtual-screen rectangle to the display that holds its centre.
fn display_bounds(rect: [i32; 4], displays: &[DisplayInfo]) -> Option<ElementBounds> {
    let [left, top, width, height] = rect;
    let width_u = u32::try_from(width).ok().filter(|width| *width > 0)?;
    let height_u = u32::try_from(height).ok().filter(|height| *height > 0)?;
    let center_x = i64::from(left) + i64::from(width) / 2;
    let center_y = i64::from(top) + i64::from(height) / 2;
    let display = displays.iter().find(|display| {
        let bounds = display.bounds;
        center_x >= i64::from(bounds.left)
            && center_x < i64::from(bounds.left) + i64::from(bounds.width)
            && center_y >= i64::from(bounds.top)
            && center_y < i64::from(bounds.top) + i64::from(bounds.height)
    })?;
    Some(ElementBounds {
        display_id: display.id.clone(),
        x: i32::try_from(i64::from(left) - i64::from(display.bounds.left)).ok()?,
        y: i32::try_from(i64::from(top) - i64::from(display.bounds.top)).ok()?,
        width: width_u,
        height: height_u,
    })
}

/// Runs one query: resolves the target and `within`, walks in the helper, checks that the
/// window is still the same one, and issues element references.
///
/// # Errors
///
/// `target_invalidated` when the target cannot be resolved or changed during the query,
/// `invalid_argument` for a bad or stale `within`, and `accessibility_unavailable` when the
/// provider could not be asked or did not answer.
pub(super) fn find_elements(
    request: &FindElementsRequest,
    control: &MutationControl,
    environment: &impl QueryEnvironment,
    refs: &ElementRefs,
) -> Result<FindElementsResult, PlatformError> {
    control.check("find_elements")?;
    request.validate()?;
    let scope = environment.scope(&request.target_ref)?;
    let within = request
        .within
        .as_deref()
        .map(|id| refs.resolve(id, &request.target_ref, &scope, "within"))
        .transpose()?;
    let query = WalkQuery {
        hwnd: scope.hwnd,
        pid: scope.pid,
        allowed_pids: scope.allowed_pids.clone(),
        within,
        name: request.name.clone(),
        role: request.role,
        max_depth: request.max_depth,
        max_elements: request.max_elements,
        timeout_ms: request.timeout_ms,
    };
    let walked = environment.walk(&query, control);
    control.check("find_elements")?;
    // The tree is the window's only if the window is still the same one after the walk, and a
    // failure of a window that closed meanwhile is that change, not a provider failure.
    if environment.scope(&request.target_ref).ok().as_ref() != Some(&scope) {
        return Err(PlatformError::TargetInvalidated {
            reason: "the window closed or changed during the query; observe again".to_owned(),
        });
    }
    let snapshot = walked?;
    let displays = environment.displays()?;
    let elements = snapshot
        .elements
        .into_iter()
        .map(|raw| ElementInfo {
            element_ref: refs.issue(&request.target_ref, &scope, &raw.runtime_id),
            role: raw.role,
            name: raw.name,
            name_truncated: raw.name_truncated,
            password: raw.password,
            depth: raw.depth,
            enabled: raw.enabled,
            focusable: raw.focusable,
            focused: raw.focused,
            offscreen: raw.offscreen,
            bounds: raw.rect.and_then(|rect| display_bounds(rect, &displays)),
            patterns: raw.patterns,
        })
        .collect();
    Ok(FindElementsResult {
        status: FindElementsResult::status_for(&snapshot.limits_reached, snapshot.provider_errors),
        truncated: !snapshot.limits_reached.is_empty(),
        limits_reached: snapshot.limits_reached,
        counts: ElementQueryCounts {
            visited: snapshot.visited,
            provider_errors: snapshot.provider_errors,
            skipped_other_process: snapshot.skipped_other_process,
        },
        elements,
    })
}

#[cfg(test)]
mod tests;
