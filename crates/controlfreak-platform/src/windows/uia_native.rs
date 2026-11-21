#![allow(unsafe_code)]

//! The UI Automation client behind `uia::Tree`. It lives on the helper's walker thread, which
//! joined the multithreaded apartment, and is never shared with another thread.

use super::desktop::{ComApartment, initialize_com};
use super::uia::{ActError, NodeFacts, Tree, TreeError};
use controlfreak_core::ElementAction;
use windows::Win32::{
    Foundation::{HWND, RECT},
    System::{
        Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, SAFEARRAY},
        Ole::{
            SafeArrayAccessData, SafeArrayCreateVector, SafeArrayDestroy, SafeArrayUnaccessData,
        },
        Variant::{VARENUM, VARIANT, VT_ARRAY, VT_BOOL, VT_I4},
    },
    UI::Accessibility::{
        CUIAutomation, CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest,
        IUIAutomationElement, IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
        IUIAutomationTogglePattern, IUIAutomationTreeWalker, TreeScope_Subtree,
        UIA_BoundingRectanglePropertyId, UIA_ControlTypePropertyId, UIA_E_ELEMENTNOTAVAILABLE,
        UIA_EditControlTypeId, UIA_ExpandCollapsePatternId, UIA_HasKeyboardFocusPropertyId,
        UIA_InvokePatternId, UIA_IsDockPatternAvailablePropertyId, UIA_IsEnabledPropertyId,
        UIA_IsExpandCollapsePatternAvailablePropertyId, UIA_IsGridItemPatternAvailablePropertyId,
        UIA_IsGridPatternAvailablePropertyId, UIA_IsInvokePatternAvailablePropertyId,
        UIA_IsItemContainerPatternAvailablePropertyId, UIA_IsKeyboardFocusablePropertyId,
        UIA_IsMultipleViewPatternAvailablePropertyId, UIA_IsOffscreenPropertyId,
        UIA_IsPasswordPropertyId, UIA_IsRangeValuePatternAvailablePropertyId,
        UIA_IsScrollItemPatternAvailablePropertyId, UIA_IsScrollPatternAvailablePropertyId,
        UIA_IsSelectionItemPatternAvailablePropertyId, UIA_IsSelectionPatternAvailablePropertyId,
        UIA_IsTableItemPatternAvailablePropertyId, UIA_IsTablePatternAvailablePropertyId,
        UIA_IsTextPatternAvailablePropertyId, UIA_IsTogglePatternAvailablePropertyId,
        UIA_IsTransformPatternAvailablePropertyId, UIA_IsValuePatternAvailablePropertyId,
        UIA_IsVirtualizedItemPatternAvailablePropertyId, UIA_IsWindowPatternAvailablePropertyId,
        UIA_NamePropertyId, UIA_PROPERTY_ID, UIA_ProcessIdPropertyId, UIA_RuntimeIdPropertyId,
        UIA_TogglePatternId,
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, IsIconic},
};
use windows::core::Interface;

/// Bounds one cross-process call inside UI Automation itself, below the helper's watchdog.
const CONNECTION_TIMEOUT_MS: u32 = 1_000;
const TRANSACTION_TIMEOUT_MS: u32 = 2_000;

/// Availability properties in the order of `ElementPattern::ALL`; the index is the pattern bit.
const PATTERN_PROPERTIES: [UIA_PROPERTY_ID; 20] = [
    UIA_IsInvokePatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
    UIA_IsValuePatternAvailablePropertyId,
    UIA_IsRangeValuePatternAvailablePropertyId,
    UIA_IsSelectionPatternAvailablePropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsScrollPatternAvailablePropertyId,
    UIA_IsScrollItemPatternAvailablePropertyId,
    UIA_IsGridPatternAvailablePropertyId,
    UIA_IsGridItemPatternAvailablePropertyId,
    UIA_IsTablePatternAvailablePropertyId,
    UIA_IsTableItemPatternAvailablePropertyId,
    UIA_IsTextPatternAvailablePropertyId,
    UIA_IsWindowPatternAvailablePropertyId,
    UIA_IsTransformPatternAvailablePropertyId,
    UIA_IsDockPatternAvailablePropertyId,
    UIA_IsMultipleViewPatternAvailablePropertyId,
    UIA_IsVirtualizedItemPatternAvailablePropertyId,
    UIA_IsItemContainerPatternAvailablePropertyId,
];

/// Properties fetched with every element in one cross-process round trip. No value property
/// is requested, so no control's text value ever reaches this process.
const FACT_PROPERTIES: [UIA_PROPERTY_ID; 10] = [
    UIA_RuntimeIdPropertyId,
    UIA_ControlTypePropertyId,
    UIA_NamePropertyId,
    UIA_IsEnabledPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_IsPasswordPropertyId,
    UIA_ProcessIdPropertyId,
    UIA_BoundingRectanglePropertyId,
];

pub(super) struct UiaClient {
    automation: IUIAutomation,
    walker: IUIAutomationTreeWalker,
    cache: IUIAutomationCacheRequest,
    _apartment: ComApartment,
}

impl UiaClient {
    /// Creates the client on the calling thread, which must be the walker thread for its life.
    pub(super) fn open() -> Result<Self, String> {
        let apartment = initialize_com().map_err(|error| error.to_string())?;
        // SAFETY: COM is initialized on this thread by `apartment`, which the client keeps alive;
        // the class and interface identifiers come from the windows crate.
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
                .or_else(|_| {
                    // SAFETY: As above; the older class exists on every supported Windows.
                    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                })
                .map_err(|error| format!("CoCreateInstance(CUIAutomation): {error}"))?;
        if let Ok(bounded) = automation.cast::<IUIAutomation2>() {
            // SAFETY: Plain setters on a live interface; failure leaves the defaults in place.
            unsafe {
                let _ = bounded.SetConnectionTimeout(CONNECTION_TIMEOUT_MS);
                let _ = bounded.SetTransactionTimeout(TRANSACTION_TIMEOUT_MS);
            }
        }
        // SAFETY: Calls on a live interface that return new owned interfaces.
        let (walker, cache) = unsafe {
            let walker = automation
                .ControlViewWalker()
                .map_err(|error| format!("ControlViewWalker: {error}"))?;
            let cache = automation
                .CreateCacheRequest()
                .map_err(|error| format!("CreateCacheRequest: {error}"))?;
            for property in FACT_PROPERTIES.iter().chain(&PATTERN_PROPERTIES) {
                cache
                    .AddProperty(*property)
                    .map_err(|error| format!("AddProperty: {error}"))?;
            }
            (walker, cache)
        };
        Ok(Self {
            automation,
            walker,
            cache,
            _apartment: apartment,
        })
    }
}

impl Tree for UiaClient {
    type Node = IUIAutomationElement;

    fn root(
        &self,
        hwnd: usize,
        within: Option<&[i32]>,
    ) -> Result<(Self::Node, NodeFacts), TreeError> {
        // SAFETY: `hwnd` is a handle value; UI Automation reports a stale one as an error. The
        // cache request is a live interface owned by `self`.
        let window = unsafe {
            self.automation
                .ElementFromHandleBuildCache(HWND(hwnd as *mut core::ffi::c_void), &self.cache)
        };
        let window = window.map_err(|error| tree_error(&error))?;
        let element = match within {
            None => window,
            Some(runtime_id) => {
                let condition = runtime_id_condition(&self.automation, runtime_id)?;
                // SAFETY: Live interfaces; the subtree scope keeps the search inside the window.
                let found = unsafe {
                    window.FindFirstBuildCache(TreeScope_Subtree, &condition, &self.cache)
                };
                match found {
                    Ok(element) => element,
                    Err(error) if error.code().is_ok() => {
                        return Err(TreeError::Gone("element not found".to_owned()));
                    }
                    Err(error) => return Err(tree_error(&error)),
                }
            }
        };
        let facts = facts(&element)?;
        Ok((element, facts))
    }

    fn first_child(&self, node: &Self::Node) -> Result<Option<(Self::Node, NodeFacts)>, TreeError> {
        // SAFETY: Live interfaces owned by this thread; the result is a new owned element.
        with_facts(unsafe {
            self.walker
                .GetFirstChildElementBuildCache(node, &self.cache)
        })
    }

    fn next_sibling(
        &self,
        node: &Self::Node,
    ) -> Result<Option<(Self::Node, NodeFacts)>, TreeError> {
        // SAFETY: Live interfaces owned by this thread; the result is a new owned element.
        with_facts(unsafe {
            self.walker
                .GetNextSiblingElementBuildCache(node, &self.cache)
        })
    }

    fn find_all(
        &self,
        hwnd: usize,
        runtime_id: &[i32],
    ) -> Result<Vec<(Self::Node, NodeFacts)>, TreeError> {
        // SAFETY: As in `root`: a stale handle is reported as an error.
        let window = unsafe {
            self.automation
                .ElementFromHandleBuildCache(HWND(hwnd as *mut core::ffi::c_void), &self.cache)
        }
        .map_err(|error| tree_error(&error))?;
        let condition = runtime_id_condition(&self.automation, runtime_id)?;
        // SAFETY: Live interfaces; the subtree scope keeps the search inside the window, and the
        // cache request fetches the facts of every match in the same round trip.
        let found =
            match unsafe { window.FindAllBuildCache(TreeScope_Subtree, &condition, &self.cache) } {
                Ok(found) => found,
                // A null array is how UI Automation says nothing matched.
                Err(error) if error.code().is_ok() => return Ok(Vec::new()),
                Err(error) => return Err(TreeError::Failed(error.to_string())),
            };
        // SAFETY: A live array returned above; indexes stay below its length.
        let length = unsafe { found.Length() }.map_err(|error| tree_error(&error))?;
        (0..length)
            .map(|index| {
                // SAFETY: As above.
                let element =
                    unsafe { found.GetElement(index) }.map_err(|error| tree_error(&error))?;
                let facts = facts(&element)?;
                Ok((element, facts))
            })
            .collect()
    }

    fn act(
        &self,
        node: &Self::Node,
        action: ElementAction,
        ready: &dyn Fn() -> Result<(), controlfreak_core::PlatformError>,
    ) -> Result<(), ActError> {
        /// The pattern interface, obtained before the last check so that a provider that is slow
        /// to hand it out cannot delay the call past that check.
        enum Pattern {
            Invoke(IUIAutomationInvokePattern),
            Toggle(IUIAutomationTogglePattern),
            ExpandCollapse(IUIAutomationExpandCollapsePattern),
        }
        let missing = |error: windows::core::Error| ActError::Refused(error.to_string());
        // SAFETY: `node` is a live element of this thread; each call returns a new owned pattern
        // interface or an error, and a null pattern is reported as an error.
        let pattern = unsafe {
            match action {
                ElementAction::Invoke => Pattern::Invoke(
                    node.GetCurrentPatternAs(UIA_InvokePatternId)
                        .map_err(missing)?,
                ),
                ElementAction::Toggle => Pattern::Toggle(
                    node.GetCurrentPatternAs(UIA_TogglePatternId)
                        .map_err(missing)?,
                ),
                ElementAction::Expand | ElementAction::Collapse => Pattern::ExpandCollapse(
                    node.GetCurrentPatternAs(UIA_ExpandCollapsePatternId)
                        .map_err(missing)?,
                ),
            }
        };
        ready().map_err(ActError::NotReady)?;
        // SAFETY: Plain methods on the live pattern interface obtained above.
        let called = unsafe {
            match (&pattern, action) {
                (Pattern::Invoke(pattern), _) => pattern.Invoke(),
                (Pattern::Toggle(pattern), _) => pattern.Toggle(),
                (Pattern::ExpandCollapse(pattern), ElementAction::Collapse) => pattern.Collapse(),
                (Pattern::ExpandCollapse(pattern), _) => pattern.Expand(),
            }
        };
        // Whatever the call returned, the provider may have changed the element before it
        // failed, so the effect of a failed call is unknown.
        called.map_err(|error| ActError::Unknown(error.to_string()))
    }

    fn in_front(&self, hwnd: usize, pid: u32) -> Result<(), controlfreak_core::PlatformError> {
        super::environment::ensure_interactive_input_desktop("act_on_element")?;
        let window = HWND(hwnd as *mut core::ffi::c_void);
        let mut owner = 0;
        // SAFETY: These calls only read state of a window handle value; `owner` is writable for
        // the synchronous call, and a stale handle reads as no owner and not minimized.
        let (foreground, minimized) = unsafe {
            GetWindowThreadProcessId(window, Some(&raw mut owner));
            (GetForegroundWindow(), IsIconic(window).as_bool())
        };
        if foreground != window || minimized || owner != pid {
            return Err(controlfreak_core::PlatformError::refused(
                controlfreak_core::RefusalCause::NotForeground,
                "the approved window is no longer in front and not minimized, so nothing was done; call focus_window on it",
            ));
        }
        Ok(())
    }
}

/// Pairs a neighbour the walker returned with its facts; a null element means none.
fn with_facts(
    element: windows::core::Result<IUIAutomationElement>,
) -> Result<Option<(IUIAutomationElement, NodeFacts)>, TreeError> {
    match element {
        Ok(element) => {
            let facts = facts(&element)?;
            Ok(Some((element, facts)))
        }
        // A null element is how a walker says there is no such neighbour.
        Err(error) if error.code().is_ok() => Ok(None),
        Err(error) => Err(tree_error(&error)),
    }
}

fn tree_error(error: &windows::core::Error) -> TreeError {
    #[allow(clippy::cast_possible_wrap)]
    let gone = error.code().0 == UIA_E_ELEMENTNOTAVAILABLE as i32;
    if gone {
        TreeError::Gone(error.to_string())
    } else {
        TreeError::Failed(error.to_string())
    }
}

/// Destroys a `SAFEARRAY` this module owns when dropped.
struct OwnedSafeArray(*mut SAFEARRAY);

impl Drop for OwnedSafeArray {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: The array was created or returned to this module as owned and is destroyed
            // exactly once, here.
            let _ = unsafe { SafeArrayDestroy(self.0) };
        }
    }
}

fn runtime_id(element: &IUIAutomationElement) -> Result<Vec<i32>, TreeError> {
    // SAFETY: The runtime ID is part of every element's identity and needs no provider call.
    let array =
        OwnedSafeArray(unsafe { element.GetRuntimeId() }.map_err(|error| tree_error(&error))?);
    if array.0.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: `array` is a live one-dimensional array of 4-byte integers (VT_I4), as UI
    // Automation documents for runtime IDs; the shape is checked before the data is read, and
    // the data is copied while the array is locked by SafeArrayAccessData.
    unsafe {
        let header = &*array.0;
        if header.cDims != 1 || header.cbElements != 4 {
            return Err(TreeError::Failed("unexpected runtime ID shape".to_owned()));
        }
        let length = usize::try_from(header.rgsabound[0].cElements).unwrap_or(0);
        if length == 0 {
            return Ok(Vec::new());
        }
        let mut data = core::ptr::null_mut();
        SafeArrayAccessData(array.0, &raw mut data).map_err(|error| tree_error(&error))?;
        let values = core::slice::from_raw_parts(data.cast::<i32>(), length).to_vec();
        let _ = SafeArrayUnaccessData(array.0);
        Ok(values)
    }
}

fn runtime_id_condition(
    automation: &IUIAutomation,
    runtime_id: &[i32],
) -> Result<windows::Win32::UI::Accessibility::IUIAutomationCondition, TreeError> {
    let length = u32::try_from(runtime_id.len())
        .map_err(|_| TreeError::Failed("runtime ID too long".to_owned()))?;
    // SAFETY: Creates an owned vector of `length` VT_I4 elements, destroyed by the guard.
    let array = OwnedSafeArray(unsafe { SafeArrayCreateVector(VT_I4, 0, length) });
    if array.0.is_null() {
        return Err(TreeError::Failed(
            "could not allocate a runtime ID".to_owned(),
        ));
    }
    // SAFETY: The array holds exactly `length` i32 slots while locked by SafeArrayAccessData.
    unsafe {
        let mut data = core::ptr::null_mut();
        SafeArrayAccessData(array.0, &raw mut data).map_err(|error| tree_error(&error))?;
        core::ptr::copy_nonoverlapping(runtime_id.as_ptr(), data.cast::<i32>(), runtime_id.len());
        let _ = SafeArrayUnaccessData(array.0);
    }
    let mut value = VARIANT::default();
    // SAFETY: A default VARIANT is VT_EMPTY and owns nothing, so overwriting its plain fields
    // drops nothing.
    unsafe {
        let fields = &mut *value.Anonymous.Anonymous;
        fields.vt = VARENUM(VT_ARRAY.0 | VT_I4.0);
        fields.Anonymous.parray = array.0;
    }
    // SAFETY: `value` is a well-formed VT_ARRAY | VT_I4 VARIANT; the windows crate's VARIANT has
    // no destructor, so nothing here clears it. The condition keeps the array: destroying it here
    // as well corrupted the heap once the condition was released (the native test below exercises
    // this), so the guard is forgotten. If a failed call did not keep it, a few bytes leak in a
    // helper process that is retired after a bounded number of queries.
    let condition = unsafe { automation.CreatePropertyCondition(UIA_RuntimeIdPropertyId, &value) };
    std::mem::forget(array);
    condition.map_err(|error| tree_error(&error))
}

fn cached_bool(element: &IUIAutomationElement, property: UIA_PROPERTY_ID) -> bool {
    // SAFETY: Reads a property from the element's local cache; a VT_BOOL VARIANT owns no
    // resources, so it needs no clearing.
    unsafe {
        element.GetCachedPropertyValue(property).is_ok_and(|value| {
            value.Anonymous.Anonymous.vt == VT_BOOL
                && value.Anonymous.Anonymous.Anonymous.boolVal.0 != 0
        })
    }
}

/// Whether the element is a password field. A failed read counts as yes. A provider that does not
/// answer the property itself gets UI Automation's default `false`, so that default is ignored
/// and an edit control without its own answer counts as one too: a name is withheld rather than
/// exposed.
fn is_password(element: &IUIAutomationElement, control_type: i32) -> bool {
    // SAFETY: As in `cached_bool`. With `ignoredefaultvalue` an unanswered property comes back
    // as the reserved not-supported value (VT_UNKNOWN), which is not read and owns no reference
    // of ours.
    let answer = unsafe {
        element
            .GetCachedPropertyValueEx(UIA_IsPasswordPropertyId, true)
            .map(|value| {
                (value.Anonymous.Anonymous.vt == VT_BOOL)
                    .then(|| value.Anonymous.Anonymous.Anonymous.boolVal.0 != 0)
            })
    };
    match answer {
        Ok(Some(password)) => password,
        Ok(None) => control_type == UIA_EditControlTypeId.0,
        Err(_) => true,
    }
}

fn facts(element: &IUIAutomationElement) -> Result<NodeFacts, TreeError> {
    let runtime_id = runtime_id(element)?;
    // SAFETY: Cached reads from a live element; each returns an owned value or an error.
    let (control_type, process_id, name, rect) = unsafe {
        (
            element
                .CachedControlType()
                .map_err(|error| tree_error(&error))?
                .0,
            element
                .CachedProcessId()
                .map_err(|error| tree_error(&error))?,
            element
                .CachedName()
                .map_err(|error| tree_error(&error))?
                .to_string(),
            element.CachedBoundingRectangle().unwrap_or_default(),
        )
    };
    let mut patterns = 0_u32;
    for (bit, property) in PATTERN_PROPERTIES.iter().enumerate() {
        if cached_bool(element, *property) {
            patterns |= 1 << bit;
        }
    }
    Ok(NodeFacts {
        runtime_id,
        control_type,
        name,
        enabled: cached_bool(element, UIA_IsEnabledPropertyId),
        focusable: cached_bool(element, UIA_IsKeyboardFocusablePropertyId),
        focused: cached_bool(element, UIA_HasKeyboardFocusPropertyId),
        offscreen: cached_bool(element, UIA_IsOffscreenPropertyId),
        password: is_password(element, control_type),
        process_id: u32::try_from(process_id).unwrap_or(0),
        rect: rect_of(rect),
        patterns,
    })
}

fn rect_of(rect: RECT) -> Option<[i32; 4]> {
    let width = rect.right.checked_sub(rect.left)?;
    let height = rect.bottom.checked_sub(rect.top)?;
    (width > 0 && height > 0).then_some([rect.left, rect.top, width, height])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::uia::{
        ActQuery, ActionEnvironment, ActionHelper, HelperEnvironment, QueryEnvironment, Snapshot,
        WalkQuery, perform, walk,
    };
    use crate::windows::uia_process::HelperPool;
    use controlfreak_core::{ElementPattern, ElementQueryLimit, ElementRole, MutationControl};
    use std::sync::atomic::AtomicBool;
    use std::{
        sync::{Mutex, mpsc},
        thread,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::{
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                MSG, PM_REMOVE, PeekMessageW, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE,
                WM_COMMAND, WNDCLASSW, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_OVERLAPPEDWINDOW, WS_VISIBLE,
            },
        },
        core::w,
    };

    /// Button notifications (`WM_COMMAND`) the fixture windows received.
    static COMMANDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    unsafe extern "system" fn fixture_procedure(
        hwnd: HWND,
        message: u32,
        wparam: windows::Win32::Foundation::WPARAM,
        lparam: windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        if message == WM_COMMAND {
            COMMANDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        // SAFETY: Forwards the arguments Windows passed to this procedure unchanged.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }

    struct WindowValue(HWND);
    // SAFETY: The handle is a plain value; UI Automation reads the window from another thread.
    unsafe impl Send for WindowValue {}

    /// A window this test process owns, with one real push button. It is visible, so UI
    /// Automation lists its child, but far off every display, out of the taskbar and never
    /// activated. Its thread pumps messages until the fixture closes, which destroys the window.
    struct Fixture {
        window: HWND,
        stop: Option<mpsc::Sender<()>>,
        owner: Option<thread::JoinHandle<()>>,
    }

    impl Fixture {
        fn open() -> Self {
            let (window_sender, window_receiver) = mpsc::channel();
            let (stop, stop_receiver) = mpsc::channel::<()>();
            let owner = thread::spawn(move || {
                // SAFETY: Registers a plain class whose procedure only calls DefWindowProcW
                // (registering it twice only fails), then creates a top-level window of it and a
                // child button, both owned by this thread and destroyed below; no pointers are
                // retained.
                let window = unsafe {
                    let instance = GetModuleHandleW(None).expect("module handle");
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(fixture_procedure),
                        hInstance: instance.into(),
                        lpszClassName: w!("ControlFreakUiaFixture"),
                        ..WNDCLASSW::default()
                    };
                    RegisterClassW(&raw const class);
                    let window = CreateWindowExW(
                        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                        w!("ControlFreakUiaFixture"),
                        w!("ControlFreak UIA fixture"),
                        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                        -20_000,
                        -20_000,
                        300,
                        200,
                        None,
                        None,
                        None,
                        None,
                    )
                    .expect("fixture window");
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("BUTTON"),
                        w!("Ping"),
                        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON.cast_unsigned()),
                        10,
                        10,
                        80,
                        30,
                        Some(window),
                        None,
                        None,
                        None,
                    )
                    .expect("fixture button");
                    window
                };
                window_sender.send(WindowValue(window)).unwrap();
                let mut message = MSG::default();
                while matches!(stop_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    // SAFETY: `message` is writable; this thread owns the windows.
                    while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool()
                    {
                        // SAFETY: `message` was filled by PeekMessageW just above.
                        unsafe { DispatchMessageW(&raw const message) };
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                // SAFETY: The window is owned by this thread and destroyed once; its child goes
                // with it.
                unsafe { DestroyWindow(window) }.expect("destroy the window");
            });
            let window = window_receiver.recv().unwrap().0;
            Self {
                window,
                stop: Some(stop),
                owner: Some(owner),
            }
        }

        fn query(&self) -> WalkQuery {
            WalkQuery {
                hwnd: self.window.0 as usize,
                pid: std::process::id(),
                allowed_pids: vec![std::process::id()],
                within: None,
                name: Some("ping".into()),
                role: Some(ElementRole::Button),
                max_depth: 8,
                max_elements: 10,
                timeout_ms: 5_000,
            }
        }

        fn close(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(owner) = self.owner.take() {
                owner.join().unwrap();
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.close();
        }
    }

    fn assert_one_ping_button(snapshot: &Snapshot) {
        assert!(snapshot.root_answered);
        assert!(
            !snapshot
                .limits_reached
                .contains(&ElementQueryLimit::Timeout)
        );
        assert_eq!(snapshot.elements.len(), 1, "{snapshot:?}");
        let button = &snapshot.elements[0];
        assert_eq!(button.name, "Ping");
        assert!(button.patterns.contains(&ElementPattern::Invoke));
        assert!(!button.runtime_id.is_empty());
    }

    /// Reads the fixture through the real UI Automation client in this process. Nothing outside
    /// this process is read.
    #[test]
    fn the_native_client_reads_a_real_button_by_role_and_name() {
        let mut fixture = Fixture::open();
        let client = UiaClient::open().expect("UI Automation client");
        let query = fixture.query();
        let out = Mutex::new(Snapshot::default());
        walk(
            &client,
            &query,
            Instant::now() + Duration::from_secs(5),
            &out,
        )
        .unwrap();
        let snapshot = out.into_inner().unwrap();
        assert_one_ping_button(&snapshot);

        // The runtime ID finds the same button again as the root of a scoped walk.
        let (_, found) = client
            .root(query.hwnd, Some(&snapshot.elements[0].runtime_id))
            .expect("the button by runtime ID");
        assert_eq!(found.name, "Ping");

        // An action finds it the same way, once. The window is this test's own, off every
        // display and never activated, so the real last check refuses it before the call.
        let pressed = COMMANDS.load(std::sync::atomic::Ordering::SeqCst);
        let action = ActQuery {
            hwnd: query.hwnd,
            pid: query.pid,
            runtime_id: snapshot.elements[0].runtime_id.clone(),
            action: ElementAction::Invoke,
            timeout_ms: 5_000,
        };
        let dispatched = AtomicBool::new(false);
        let refused = perform(&client, &action, &AtomicBool::new(false), &dispatched).unwrap_err();
        assert_eq!(refused.reason_code(), Some("not_foreground"));
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(COMMANDS.load(std::sync::atomic::Ordering::SeqCst), pressed);

        // With only that check passed over, the real pattern call presses the button. Nothing
        // here makes the window the foreground window.
        let in_front_for_test = InFront(&client);
        perform(
            &in_front_for_test,
            &action,
            &AtomicBool::new(false),
            &dispatched,
        )
        .expect("invoke");
        assert!(dispatched.load(std::sync::atomic::Ordering::SeqCst));
        let deadline = Instant::now() + Duration::from_secs(2);
        while COMMANDS.load(std::sync::atomic::Ordering::SeqCst) == pressed {
            assert!(
                Instant::now() < deadline,
                "the button never reported a press"
            );
            thread::sleep(Duration::from_millis(10));
        }
        // The button has no toggle pattern, so nothing is sent for a toggle.
        let toggle = ActQuery {
            action: ElementAction::Toggle,
            ..action
        };
        let dispatched = AtomicBool::new(false);
        assert!(
            perform(
                &in_front_for_test,
                &toggle,
                &AtomicBool::new(false),
                &dispatched
            )
            .is_err()
        );
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));

        fixture.close();
        // A closed window has no tree to read.
        assert!(client.root(query.hwnd, None).is_err());
    }

    /// The real client with its foreground check passed over, so a test can reach the real
    /// pattern call on a window that is never activated.
    struct InFront<'a>(&'a UiaClient);

    impl Tree for InFront<'_> {
        type Node = IUIAutomationElement;
        fn root(
            &self,
            hwnd: usize,
            within: Option<&[i32]>,
        ) -> Result<(Self::Node, NodeFacts), TreeError> {
            self.0.root(hwnd, within)
        }
        fn first_child(
            &self,
            node: &Self::Node,
        ) -> Result<Option<(Self::Node, NodeFacts)>, TreeError> {
            self.0.first_child(node)
        }
        fn next_sibling(
            &self,
            node: &Self::Node,
        ) -> Result<Option<(Self::Node, NodeFacts)>, TreeError> {
            self.0.next_sibling(node)
        }
        fn find_all(
            &self,
            hwnd: usize,
            runtime_id: &[i32],
        ) -> Result<Vec<(Self::Node, NodeFacts)>, TreeError> {
            self.0.find_all(hwnd, runtime_id)
        }
        fn act(
            &self,
            node: &Self::Node,
            action: ElementAction,
            ready: &dyn Fn() -> Result<(), controlfreak_core::PlatformError>,
        ) -> Result<(), ActError> {
            self.0.act(node, action, ready)
        }
        fn in_front(&self, _: usize, _: u32) -> Result<(), controlfreak_core::PlatformError> {
            Ok(())
        }
    }

    /// Serves real UI Automation queries; started only as a child of the helper test below.
    #[test]
    #[ignore = "spawned only by the helper process test"]
    fn uia_uia_process() {
        crate::windows::uia::serve_uia_helper(std::io::stdin(), std::io::stdout().lock()).unwrap();
    }

    /// The real helper process, started from this test binary, reads a window of this process
    /// through the same pool, IPC and deadlines the server uses, and stays warm between queries.
    #[test]
    fn the_uia_process_reads_a_window_through_ipc() {
        let fixture = Fixture::open();
        let command = crate::OcrHelperCommand::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("--exact")
            .arg("windows::uia_native::tests::uia_uia_process")
            .arg("--nocapture")
            .arg("--quiet");
        let pool = HelperPool::default();
        let answers = Mutex::new(Vec::new());
        let record = |outcome: Result<(), String>| answers.lock().unwrap().push(outcome);
        let environment = HelperEnvironment {
            command: &command,
            pool: &pool,
            answered: &record,
        };
        let control = MutationControl::default();
        let mut runtime_id = Vec::new();
        for _ in 0..2 {
            let snapshot = environment.walk(&fixture.query(), &control).unwrap();
            assert_one_ping_button(&snapshot);
            runtime_id.clone_from(&snapshot.elements[0].runtime_id);
        }
        assert_eq!(*answers.lock().unwrap(), [Ok(()), Ok(())]);
        assert!(pool.idle.lock().unwrap().is_some(), "the helper stays warm");

        // The same warm helper finds the button for an action request, then checks the window
        // itself right before the call. The fixture is never in front, so the helper answers
        // that it did not act, and stays warm.
        let actions = ActionHelper {
            helper: environment,
        };
        let sent = std::cell::Cell::new(0);
        let query = ActQuery {
            hwnd: fixture.window.0 as usize,
            pid: std::process::id(),
            runtime_id,
            action: ElementAction::Invoke,
            timeout_ms: 5_000,
        };
        let failure = actions
            .act(&query, &control, &|| sent.set(sent.get() + 1))
            .expect_err("the fixture is not in front");
        assert_eq!(sent.get(), 1);
        assert_eq!(failure.error.reason_code(), Some("not_foreground"));
        assert!(!failure.dispatched, "{failure:?}");
        assert!(pool.idle.lock().unwrap().is_some(), "the helper stays warm");
    }
}
