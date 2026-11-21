//! Fakes of the UI Automation provider: a normal tree, a hung provider, a password field, a
//! tree deeper and wider than the limits, a window that closes during the query, and a foreign
//! process inside the window.

use super::*;
use controlfreak_core::{DisplayBounds, ElementQueryStatus};

const PID: u32 = 40;
const OTHER_PID: u32 = 41;
const HWND: usize = 0x1234;

#[derive(Clone)]
struct FakeNode {
    facts: NodeFacts,
    children: Vec<usize>,
    /// Asking for this node's first child blocks this long (a hung provider).
    hang: Option<Duration>,
    /// Asking for this node's first child fails (a provider error).
    fails: bool,
}

/// A tree of nodes by index; node 0 is the window. Node 1 is the window's sibling, which a walk
/// must never reach.
struct FakeTree {
    nodes: Vec<FakeNode>,
    parents: Vec<Option<usize>>,
    calls: Mutex<Vec<usize>>,
    /// The window is gone: the root fails.
    root_gone: bool,
}

fn facts(id: i32, control_type: i32, name: &str) -> NodeFacts {
    NodeFacts {
        runtime_id: vec![42, id],
        control_type,
        name: name.to_owned(),
        enabled: true,
        focusable: control_type == 50000,
        focused: false,
        offscreen: false,
        password: false,
        process_id: PID,
        rect: Some([100 + id * 10, 200, 80, 24]),
        patterns: 1,
    }
}

fn node(facts: NodeFacts, children: Vec<usize>) -> FakeNode {
    FakeNode {
        facts,
        children,
        hang: None,
        fails: false,
    }
}

impl FakeTree {
    fn new(nodes: Vec<FakeNode>) -> Self {
        let mut parents = vec![None; nodes.len()];
        for (index, node) in nodes.iter().enumerate() {
            for child in &node.children {
                parents[*child] = Some(index);
            }
        }
        Self {
            nodes,
            parents,
            calls: Mutex::new(Vec::new()),
            root_gone: false,
        }
    }

    /// A window (0) with a sibling (1) outside it, a toolbar, three rows with an "Approve"
    /// button each, and a password field.
    fn normal() -> Self {
        let mut password = facts(10, 50004, "hunter2");
        password.password = true;
        Self::new(vec![
            node(facts(0, 50032, "Orders"), vec![2, 3, 9]),
            node(facts(1, 50032, "Another window"), vec![]),
            node(facts(2, 50021, "Tools"), vec![]),
            node(facts(3, 50008, "Orders list"), vec![4, 5, 6]),
            node(facts(4, 50029, "Row 1"), vec![7]),
            node(facts(5, 50029, "Row 2"), vec![8]),
            node(facts(6, 50029, "Row 3"), vec![10]),
            node(facts(7, 50000, "Approve"), vec![]),
            node(facts(8, 50000, "Approve"), vec![]),
            node(password, vec![]),
            node(facts(11, 50000, "Approve"), vec![]),
        ])
    }

    fn sibling_of(&self, index: usize) -> Option<usize> {
        let parent = self.parents[index]?;
        let siblings = &self.nodes[parent].children;
        let position = siblings.iter().position(|child| *child == index)?;
        siblings.get(position + 1).copied()
    }
}

impl Tree for FakeTree {
    type Node = usize;

    fn root(&self, hwnd: usize, within: Option<&[i32]>) -> Result<(usize, NodeFacts), TreeError> {
        assert_eq!(hwnd, HWND);
        if self.root_gone {
            return Err(TreeError::Gone("window destroyed".into()));
        }
        // Like FindFirst with a subtree scope: only node 0 and its descendants are searched.
        let mut pending = vec![0];
        while let Some(index) = pending.pop() {
            if within.is_none_or(|id| self.nodes[index].facts.runtime_id == id) {
                return Ok((index, self.nodes[index].facts.clone()));
            }
            pending.extend(&self.nodes[index].children);
        }
        Err(TreeError::Gone("not found".into()))
    }

    fn first_child(&self, node: &usize) -> Result<Option<(usize, NodeFacts)>, TreeError> {
        self.calls.lock().unwrap().push(*node);
        let entry = &self.nodes[*node];
        if let Some(hang) = entry.hang {
            thread::sleep(hang);
        }
        if entry.fails {
            return Err(TreeError::Failed("provider error".into()));
        }
        Ok(entry
            .children
            .first()
            .map(|child| (*child, self.nodes[*child].facts.clone())))
    }

    fn next_sibling(&self, node: &usize) -> Result<Option<(usize, NodeFacts)>, TreeError> {
        assert_ne!(*node, 0, "the walk asked for the window's sibling");
        self.calls.lock().unwrap().push(*node);
        Ok(self
            .sibling_of(*node)
            .map(|sibling| (sibling, self.nodes[sibling].facts.clone())))
    }
}

fn query() -> WalkQuery {
    WalkQuery {
        hwnd: HWND,
        pid: PID,
        allowed_pids: vec![PID],
        within: None,
        name: None,
        role: None,
        max_depth: 32,
        max_elements: 100,
        timeout_ms: 2_000,
    }
}

fn run(tree: &FakeTree, query: &WalkQuery) -> Result<Snapshot, PlatformError> {
    let out = Mutex::new(Snapshot::default());
    let deadline = Instant::now() + Duration::from_millis(u64::from(query.timeout_ms));
    walk(tree, query, deadline, &out)?;
    Ok(out.into_inner().unwrap())
}

fn names(snapshot: &Snapshot) -> Vec<&str> {
    snapshot
        .elements
        .iter()
        .map(|element| element.name.as_str())
        .collect()
}

#[test]
fn a_normal_tree_is_read_in_document_order_without_leaving_the_window() {
    let snapshot = run(&FakeTree::normal(), &query()).unwrap();
    assert_eq!(
        names(&snapshot),
        [
            "Orders",
            "Tools",
            "Orders list",
            "Row 1",
            "Approve",
            "Row 2",
            "Approve",
            "Row 3",
            "Approve",
            ""
        ]
    );
    assert!(!names(&snapshot).contains(&"Another window"));
    assert!(snapshot.root_answered && snapshot.limits_reached.is_empty());
    assert_eq!(snapshot.provider_errors, 0);
    assert_eq!(snapshot.visited, 10);
    let button = &snapshot.elements[4];
    assert_eq!(button.role, ElementRole::Button);
    assert_eq!(button.depth, 3);
    assert_eq!(button.patterns, [ElementPattern::Invoke]);
    assert!(button.enabled && button.focusable);
}

#[test]
fn a_password_field_never_returns_its_name_and_cannot_be_probed_by_name() {
    let snapshot = run(&FakeTree::normal(), &query()).unwrap();
    let field = snapshot
        .elements
        .iter()
        .find(|element| element.password)
        .unwrap();
    assert_eq!(field.name, "");
    assert_eq!(field.role, ElementRole::Edit);
    let text = serde_json::to_string(&snapshot).unwrap();
    assert!(!text.contains("hunter2"));

    let probe = WalkQuery {
        name: Some("hunter".into()),
        ..query()
    };
    assert!(
        run(&FakeTree::normal(), &probe)
            .unwrap()
            .elements
            .is_empty()
    );
}

#[test]
fn filters_select_by_role_and_case_insensitive_name() {
    let buttons = WalkQuery {
        role: Some(ElementRole::Button),
        name: Some("approve".into()),
        ..query()
    };
    let snapshot = run(&FakeTree::normal(), &buttons).unwrap();
    assert_eq!(names(&snapshot), ["Approve", "Approve", "Approve"]);
    // The filter selects what is returned, not what is read.
    assert_eq!(snapshot.visited, 10);
}

#[test]
fn within_reads_only_that_elements_subtree() {
    let row = WalkQuery {
        within: Some(vec![42, 5]),
        ..query()
    };
    let snapshot = run(&FakeTree::normal(), &row).unwrap();
    assert_eq!(names(&snapshot), ["Row 2", "Approve"]);
    assert_eq!(snapshot.elements[0].depth, 0);

    let missing = WalkQuery {
        within: Some(vec![42, 99]),
        ..query()
    };
    assert!(matches!(
        run(&FakeTree::normal(), &missing),
        Err(PlatformError::InvalidArgument { argument, .. }) if argument == "within"
    ));
}

#[test]
fn a_deep_and_wide_tree_stops_at_each_limit_and_says_which() {
    // A chain 0 -> 2 -> 3 -> ... 99 levels deep.
    let mut nodes = vec![node(facts(0, 50032, "Root"), vec![2])];
    nodes.push(node(facts(1, 50032, "Sibling"), vec![]));
    for level in 2..100 {
        let children = if level < 99 { vec![level + 1] } else { vec![] };
        nodes.push(node(
            facts(i32::try_from(level).unwrap(), 50026, "Level"),
            children,
        ));
    }
    let shallow = WalkQuery {
        max_depth: 3,
        ..query()
    };
    let snapshot = run(&FakeTree::new(nodes.clone()), &shallow).unwrap();
    assert_eq!(snapshot.elements.len(), 4);
    assert_eq!(snapshot.limits_reached, [ElementQueryLimit::MaxDepth]);

    let few = WalkQuery {
        max_elements: 5,
        ..query()
    };
    let snapshot = run(&FakeTree::new(nodes.clone()), &few).unwrap();
    assert_eq!(snapshot.elements.len(), 5);
    assert_eq!(snapshot.limits_reached, [ElementQueryLimit::MaxElements]);

    // Exactly as many matches as allowed, within the depth limit, is complete.
    let exact = WalkQuery {
        max_elements: 31,
        max_depth: 64,
        ..query()
    };
    let mut chain = nodes[..31].to_vec();
    chain[30].children.clear();
    assert!(
        run(&FakeTree::new(chain), &exact)
            .unwrap()
            .limits_reached
            .is_empty()
    );

    // Long names push the result past its byte budget.
    let long = "x".repeat(MAX_NAME_CHARS * 2);
    let mut wide = vec![node(facts(0, 50032, "Root"), (2..2_002).collect())];
    wide.push(node(facts(1, 50032, "Sibling"), vec![]));
    for index in 2..2_002 {
        wide.push(node(facts(index, 50020, &long), vec![]));
    }
    let many = WalkQuery {
        max_elements: 500,
        ..query()
    };
    let snapshot = run(&FakeTree::new(wide), &many).unwrap();
    assert_eq!(snapshot.limits_reached, [ElementQueryLimit::ResponseBytes]);
    assert!(snapshot.bytes <= MAX_RESPONSE_BYTES);
    assert!(snapshot.elements.iter().skip(1).all(|element| element.name_truncated
        && element.name.chars().count() == MAX_NAME_CHARS));
}

#[test]
fn the_node_limit_bounds_a_huge_flat_tree() {
    let count = usize::try_from(NODE_LIMIT).unwrap() + 10;
    let mut nodes = vec![node(facts(0, 50008, "List"), (2..count).collect())];
    nodes.push(node(facts(1, 50032, "Sibling"), vec![]));
    for index in 2..count {
        nodes.push(node(
            facts(i32::try_from(index).unwrap(), 50007, "Item"),
            vec![],
        ));
    }
    let buttons = WalkQuery {
        role: Some(ElementRole::Button),
        ..query()
    };
    let snapshot = run(&FakeTree::new(nodes), &buttons).unwrap();
    assert_eq!(snapshot.visited, NODE_LIMIT);
    assert_eq!(snapshot.limits_reached, [ElementQueryLimit::NodeLimit]);
}

#[test]
fn a_provider_error_loses_one_subtree_and_marks_the_result_partial() {
    let mut tree = FakeTree::normal();
    tree.nodes[4].fails = true;
    let snapshot = run(&tree, &query()).unwrap();
    assert_eq!(snapshot.provider_errors, 1);
    assert!(names(&snapshot).contains(&"Row 2"));
    assert_eq!(
        names(&snapshot)
            .iter()
            .filter(|name| **name == "Approve")
            .count(),
        2
    );
    assert_eq!(
        FindElementsResult::status_for(&snapshot.limits_reached, snapshot.provider_errors),
        ElementQueryStatus::Partial
    );
}

#[test]
fn a_closed_window_is_unavailable_never_an_empty_tree() {
    let mut tree = FakeTree::normal();
    tree.root_gone = true;
    match run(&tree, &query()) {
        Err(PlatformError::Failed { cause, .. }) => {
            assert_eq!(cause, FailureCause::AccessibilityUnavailable);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn elements_of_another_process_are_skipped_with_their_subtrees() {
    let mut tree = FakeTree::normal();
    tree.nodes[3].facts.process_id = OTHER_PID;
    let snapshot = run(&tree, &query()).unwrap();
    assert_eq!(names(&snapshot), ["Orders", "Tools", ""]);
    assert_eq!(snapshot.skipped_other_process, 1);
    assert!(!tree.calls.lock().unwrap().contains(&4));

    let hosted = WalkQuery {
        allowed_pids: vec![PID, OTHER_PID],
        ..query()
    };
    assert_eq!(run(&tree, &hosted).unwrap().elements.len(), 10);

    // A root that now belongs to another process is the target changing.
    tree.nodes[0].facts.process_id = OTHER_PID;
    assert!(matches!(
        run(&tree, &query()),
        Err(PlatformError::TargetInvalidated { .. })
    ));
}

#[test]
fn the_deadline_stops_a_slow_walk_with_what_it_read() {
    let mut tree = FakeTree::normal();
    tree.nodes[3].hang = Some(Duration::from_millis(250));
    let slow = WalkQuery {
        timeout_ms: 100,
        ..query()
    };
    let snapshot = run(&tree, &slow).unwrap();
    assert_eq!(snapshot.limits_reached, [ElementQueryLimit::Timeout]);
    assert!(names(&snapshot).contains(&"Orders list"));
    assert!(!names(&snapshot).contains(&"Row 1"));
}

fn request_line(id: u64, query: &WalkQuery) -> Vec<u8> {
    let mut line = serde_json::to_vec(&UiaHelperRequest {
        id,
        query: query.clone(),
    })
    .unwrap();
    line.push(b'\n');
    line
}

fn replies(output: &[u8]) -> Vec<UiaHelperReply> {
    output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

/// Serves `lines` in a helper with open input, collects `count` replies, then closes input.
fn serve_lines<T: Tree>(
    lines: &[Vec<u8>],
    count: usize,
    open: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Vec<UiaHelperReply> {
    let (input, mut requests) = std::io::pipe().unwrap();
    let (output, writer) = std::io::pipe().unwrap();
    let helper = thread::spawn(move || {
        serve_uia_helper_with(input, writer, open, Duration::from_secs(30)).unwrap();
    });
    for line in lines {
        requests.write_all(line).unwrap();
    }
    let mut output = BufReader::new(output);
    let replies = (0..count)
        .map(|_| {
            let line = read_bounded_line(&mut output, 1 << 22).unwrap().unwrap();
            serde_json::from_slice(&line).unwrap()
        })
        .collect();
    drop(requests);
    helper.join().unwrap();
    replies
}

#[test]
fn the_helper_answers_requests_in_order_and_keeps_serving() {
    let replies = serve_lines(
        &[request_line(3, &query()), request_line(4, &query())],
        2,
        || Ok(FakeTree::normal()),
    );
    assert_eq!(
        replies.iter().map(|reply| reply.id).collect::<Vec<_>>(),
        [3, 4]
    );
    assert!(
        replies
            .iter()
            .all(|reply| reply.ready && reply.keeps_helper())
    );
}

/// Never finishes creating its UI Automation client.
fn hung_client() -> Result<FakeTree, String> {
    thread::sleep(Duration::from_secs(5));
    Err("late".to_owned())
}

#[test]
fn a_client_that_does_not_start_is_not_ready_and_retires_the_helper() {
    let hung = WalkQuery {
        timeout_ms: 100,
        ..query()
    };
    let (reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(&request_line(5, &hung)).unwrap();
    let mut output = Vec::new();
    serve_uia_helper_with(reader, &mut output, hung_client, Duration::from_secs(30)).unwrap();
    let reply = replies(&output).remove(0);
    assert!(!reply.ready && reply.retire);
    assert!(matches!(reply.response, UiaHelperResponse::Error { .. }));
    drop(writer);
}

#[test]
fn closing_input_ends_the_helper_without_waiting_for_a_hung_walk() {
    let mut tree = FakeTree::normal();
    tree.nodes[3].hang = Some(Duration::from_secs(5));
    let started = Instant::now();
    let mut output = Vec::new();
    serve_uia_helper_with(
        std::io::Cursor::new(request_line(1, &query())),
        &mut output,
        move || Ok(tree),
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_hung_provider_is_answered_by_the_watchdog_as_partial_and_retires_the_helper() {
    let mut tree = FakeTree::normal();
    // The provider blocks inside one call, far past the deadline.
    tree.nodes[3].hang = Some(Duration::from_secs(5));
    let hung = WalkQuery {
        timeout_ms: 200,
        ..query()
    };
    let (reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(&request_line(1, &hung)).unwrap();
    let mut output = Vec::new();
    let started = Instant::now();
    serve_uia_helper_with(
        reader,
        &mut output,
        move || Ok(tree),
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    let reply = replies(&output).remove(0);
    assert!(reply.retire && !reply.keeps_helper());
    assert!(
        reply.ready,
        "the client was created before the provider hung"
    );
    let UiaHelperResponse::Success { snapshot } = reply.response else {
        panic!("a hung provider after the root is partial, not an error");
    };
    assert!(
        snapshot
            .limits_reached
            .contains(&ElementQueryLimit::Timeout)
    );
    assert_eq!(snapshot.provider_errors, 1);
    assert!(names(&snapshot).contains(&"Orders list"));
    drop(writer);
}

/// Blocks in `root` itself.
struct HungRoot;

impl Tree for HungRoot {
    type Node = ();
    fn root(&self, _: usize, _: Option<&[i32]>) -> Result<((), NodeFacts), TreeError> {
        thread::sleep(Duration::from_secs(5));
        Err(TreeError::Failed("late".into()))
    }
    fn first_child(&self, (): &()) -> Result<Option<((), NodeFacts)>, TreeError> {
        Ok(None)
    }
    fn next_sibling(&self, (): &()) -> Result<Option<((), NodeFacts)>, TreeError> {
        Ok(None)
    }
}

#[test]
fn a_provider_that_never_answers_the_root_is_unavailable() {
    let hung = WalkQuery {
        timeout_ms: 100,
        ..query()
    };
    let (reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(&request_line(9, &hung)).unwrap();
    let mut output = Vec::new();
    serve_uia_helper_with(
        reader,
        &mut output,
        || Ok(HungRoot),
        Duration::from_secs(30),
    )
    .unwrap();
    let reply = replies(&output).remove(0);
    assert!(reply.retire);
    assert!(matches!(
        reply.response,
        UiaHelperResponse::Error {
            error: PlatformError::Failed {
                cause: FailureCause::AccessibilityUnavailable,
                ..
            }
        }
    ));
    drop(writer);
}

#[test]
fn a_client_that_cannot_start_fails_every_request_without_an_empty_tree() {
    let replies = serve_lines(
        &[request_line(1, &query()), request_line(2, &query())],
        1,
        || Err::<FakeTree, _>("class not registered".to_owned()),
    );
    for reply in replies {
        assert!(!reply.ready);
        match reply.response {
            UiaHelperResponse::Error {
                error: PlatformError::Failed { reason, .. },
            } => assert!(reason.starts_with(CLIENT_START_FAILED)),
            UiaHelperResponse::Success { .. } => panic!("an unavailable client returned a tree"),
            UiaHelperResponse::Error { error } => panic!("{error:?}"),
        }
    }
}

/// A desktop with one display and a scripted target that can close or change mid-query.
struct FakeEnvironment {
    tree: FakeTree,
    scopes: Mutex<VecDeque<Result<Scope, PlatformError>>>,
}

fn scope(created: u64) -> Scope {
    Scope {
        hwnd: HWND,
        pid: PID,
        created,
        allowed_pids: vec![PID],
    }
}

impl FakeEnvironment {
    fn new(scopes: Vec<Result<Scope, PlatformError>>) -> Self {
        Self {
            tree: FakeTree::normal(),
            scopes: Mutex::new(scopes.into()),
        }
    }
}

impl QueryEnvironment for FakeEnvironment {
    fn scope(&self, _: &str) -> Result<Scope, PlatformError> {
        let mut scopes = self.scopes.lock().unwrap();
        if scopes.len() > 1 {
            scopes.pop_front().unwrap()
        } else {
            scopes.front().unwrap().clone()
        }
    }

    fn displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        Ok(vec![
            DisplayInfo {
                id: "left".into(),
                name: "left".into(),
                bounds: DisplayBounds {
                    left: -1_920,
                    top: 0,
                    width: 1_920,
                    height: 1_080,
                },
                is_primary: false,
            },
            DisplayInfo {
                id: "primary".into(),
                name: "primary".into(),
                bounds: DisplayBounds {
                    left: 0,
                    top: 0,
                    width: 2_560,
                    height: 1_440,
                },
                is_primary: true,
            },
        ])
    }

    fn walk(&self, query: &WalkQuery, _: &MutationControl) -> Result<Snapshot, PlatformError> {
        run(&self.tree, query)
    }
}

fn request() -> FindElementsRequest {
    FindElementsRequest {
        target_ref: "target-a".into(),
        within: None,
        name: None,
        role: None,
        max_depth: 32,
        max_elements: 100,
        timeout_ms: 2_000,
    }
}

#[test]
fn results_map_bounds_to_the_display_and_issue_reusable_references() {
    let environment = FakeEnvironment::new(vec![Ok(scope(7))]);
    let refs = ElementRefs::default();
    let control = MutationControl::default();
    let result = find_elements(&request(), &control, &environment, &refs).unwrap();
    assert_eq!(result.status, ElementQueryStatus::Complete);
    assert!(!result.truncated);
    let row = result.elements.iter().find(|e| e.name == "Row 2").unwrap();
    assert_eq!(
        row.bounds,
        Some(ElementBounds {
            display_id: "primary".into(),
            x: 150,
            y: 200,
            width: 80,
            height: 24,
        })
    );
    assert!(row.element_ref.starts_with("element-"));

    // Querying again names the same element with the same reference.
    let again = find_elements(&request(), &control, &environment, &refs).unwrap();
    assert_eq!(
        again
            .elements
            .iter()
            .find(|e| e.name == "Row 2")
            .unwrap()
            .element_ref,
        row.element_ref
    );

    let scoped = FindElementsRequest {
        within: Some(row.element_ref.clone()),
        role: Some(ElementRole::Button),
        ..request()
    };
    let inside = find_elements(&scoped, &control, &environment, &refs).unwrap();
    assert_eq!(inside.elements.len(), 1);
    assert_eq!(inside.elements[0].name, "Approve");
}

#[test]
fn bounds_on_a_display_left_of_the_primary_stay_display_local() {
    let displays = FakeEnvironment::new(vec![Ok(scope(7))]).displays().unwrap();
    let bounds = display_bounds([-1_900, 10, 100, 20], &displays).unwrap();
    assert_eq!(
        (bounds.display_id.as_str(), bounds.x, bounds.y),
        ("left", 20, 10)
    );
    assert!(display_bounds([0, 0, 0, 10], &displays).is_none());
    assert!(display_bounds([10_000, 0, 10, 10], &displays).is_none());
}

#[test]
fn a_window_that_closes_during_the_query_invalidates_the_result() {
    let closed = FakeEnvironment::new(vec![
        Ok(scope(7)),
        Err(PlatformError::TargetInvalidated {
            reason: "window destroyed".into(),
        }),
    ]);
    let refs = ElementRefs::default();
    assert!(matches!(
        find_elements(&request(), &MutationControl::default(), &closed, &refs),
        Err(PlatformError::TargetInvalidated { .. })
    ));
    // A walk that fails because the window went away is that change, not a provider failure.
    let mut gone = FakeEnvironment::new(vec![
        Ok(scope(7)),
        Err(PlatformError::TargetInvalidated {
            reason: "window destroyed".into(),
        }),
    ]);
    gone.tree.root_gone = true;
    assert!(matches!(
        find_elements(&request(), &MutationControl::default(), &gone, &refs),
        Err(PlatformError::TargetInvalidated { .. })
    ));
    // A process that restarted under the same window handle is a different scope too.
    let restarted = FakeEnvironment::new(vec![Ok(scope(7)), Ok(scope(8))]);
    assert!(matches!(
        find_elements(&request(), &MutationControl::default(), &restarted, &refs),
        Err(PlatformError::TargetInvalidated { .. })
    ));
}

#[test]
fn element_references_are_refused_for_another_target_or_process_and_never_guessed() {
    let refs = ElementRefs::default();
    let control = MutationControl::default();
    let first = FakeEnvironment::new(vec![Ok(scope(7))]);
    let result = find_elements(&request(), &control, &first, &refs).unwrap();
    let element = result.elements[1].element_ref.clone();

    let other_target = FindElementsRequest {
        target_ref: "target-b".into(),
        within: Some(element.clone()),
        ..request()
    };
    let restarted = FakeEnvironment::new(vec![Ok(scope(8))]);
    let same_target = FindElementsRequest {
        within: Some(element),
        ..request()
    };
    let unknown = FindElementsRequest {
        within: Some("element-made-up".into()),
        ..request()
    };
    for (request, environment) in [
        (other_target, &first),
        (same_target, &restarted),
        (unknown, &first),
    ] {
        assert!(matches!(
            find_elements(&request, &control, environment, &refs),
            Err(PlatformError::InvalidArgument { argument, .. }) if argument == "within"
        ));
    }
}

#[test]
fn invalid_limits_are_refused_before_the_provider_is_asked() {
    let environment = FakeEnvironment::new(vec![Ok(scope(7))]);
    let bad = FindElementsRequest {
        max_elements: 0,
        ..request()
    };
    assert!(matches!(
        find_elements(
            &bad,
            &MutationControl::default(),
            &environment,
            &ElementRefs::default()
        ),
        Err(PlatformError::InvalidArgument { .. })
    ));
    assert!(environment.tree.calls.lock().unwrap().is_empty());
}
