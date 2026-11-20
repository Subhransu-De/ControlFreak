//! The tool catalog: one row per tool. A tool missing here does not exist. `tools/list`,
//! `--print-tools`, dispatch, admission, the annotations and the indicator's reading state are
//! all derived from these rows, so adding a tool means adding a row, its handler and its schema
//! builder, and documenting it in docs/tools.md (a test compares that table with this one).

use super::{
    BEGIN_CONTROL_SESSION, CAPTURE_DISPLAY, CAPTURE_REGION, CAPTURE_VISUAL_BASELINE,
    CAPTURE_WINDOW, CLICK_MOUSE, CLICK_TEXT, DRAG_MOUSE, END_CONTROL_SESSION, FIND_ELEMENTS,
    FIND_TEXT_ON_SCREEN, FOCUS_WINDOW, GET_SERVER_STATUS, InputPrepare, LIST_DISPLAYS,
    LIST_VIRTUAL_DESKTOPS, LIST_WINDOWS, MOVE_MOUSE, PRESS_KEYS, READ_TEXT_IN_REGION, RUN_ACTIONS,
    SCROLL_MOUSE, STOP_DESKTOP_WORK, SWITCH_VIRTUAL_DESKTOP, TYPE_TEXT, ToolHandler,
    WAIT_FOR_CHANGE_SINCE, WAIT_FOR_TEXT, WAIT_FOR_VISUAL_CHANGE, WAIT_FOR_WINDOW,
    handlers::{
        call_capture_display, call_capture_region, call_capture_visual_baseline,
        call_capture_window, call_find_elements, call_find_text_on_screen, call_list_displays,
        call_list_virtual_desktops, call_list_windows, call_read_text_in_region,
        call_wait_for_change_since, call_wait_for_text, call_wait_for_visual_change,
        call_wait_for_window, prepare_click_mouse, prepare_click_text, prepare_drag_mouse,
        prepare_focus_window, prepare_move_mouse, prepare_press_keys, prepare_run_actions,
        prepare_scroll_mouse, prepare_switch_virtual_desktop, prepare_type_text,
    },
    schema::{self, Contract},
};

/// Wraps a tool function as a [`ToolHandler`].
macro_rules! handler {
    ($function:path) => {
        |backend, arguments, lease| Box::pin($function(backend, arguments, lease))
    };
}

/// How a request for a tool is admitted and run.
#[derive(Clone, Copy)]
pub(super) enum Route {
    /// Cancels all desktop work and closes admission.
    Stop,
    /// Reports server state, even after a stop.
    Status,
    /// Releases desktop control.
    EndSession,
    /// Reserves desktop control for one approved window.
    BeginSession,
    /// A short read of the desktop. Stays available after a stop, with request cancellation only.
    Observe(ToolHandler),
    /// A read that can wait for seconds. A stop cancels it while it runs, but a new one is still
    /// admitted after a stop, because reading the screen is safe.
    Watch(ToolHandler),
    /// Changes the desktop. Needs admission and runs under an operation lease, which is taken only
    /// after the request is prepared, so a malformed request never reserves the desktop.
    Input(InputPrepare),
}

/// Whether the published input schema gets the approved-window `target_ref` field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TargetRef {
    /// No `target_ref`, or the builder declares it itself (as the `window_id` alias).
    None,
    /// `target_ref` is required.
    Required,
    /// `target_ref` is accepted and may be omitted.
    Optional,
}

/// One tool.
pub(super) struct ToolSpec {
    /// The stable tool name.
    pub(super) name: &'static str,
    /// The human-readable title, published as the tool title and the annotation title.
    pub(super) title: &'static str,
    pub(super) route: Route,
    /// Whether repeating the same call has no further effect (`idempotentHint`).
    pub(super) idempotent: bool,
    pub(super) target_ref: TargetRef,
    /// Reads what the screen shows: its pixels, or the controls of a window. Such a call claims
    /// no session; the indicator only shows a brief "reading the screen" state for it.
    pub(super) reads_screen: bool,
    /// Writes the description and the input and output schemas.
    pub(super) schema: fn() -> Contract,
}

impl ToolSpec {
    /// Changes the desktop, so it runs under an operation lease and reports input progress.
    pub(super) const fn is_mutating(&self) -> bool {
        matches!(self.route, Route::Input(_))
    }

    /// The kind docs/tools.md shows for the tool.
    #[cfg(test)]
    pub(super) const fn documented_kind(&self) -> &'static str {
        match self.route {
            Route::Status | Route::Observe(_) | Route::Watch(_) => "Observe",
            Route::Input(_) => "Input",
            Route::Stop | Route::EndSession | Route::BeginSession => "Session",
        }
    }
}

/// Every tool, in the order `tools/list` publishes them: the tools an agent needs first, and
/// the emergency stop last.
pub(super) static CATALOG: &[ToolSpec] = &[
    ToolSpec {
        name: GET_SERVER_STATUS,
        title: "Server status",
        route: Route::Status,
        idempotent: true,
        target_ref: TargetRef::None,
        reads_screen: false,
        schema: schema::get_server_status,
    },
    ToolSpec {
        name: BEGIN_CONTROL_SESSION,
        title: "Begin control session",
        route: Route::BeginSession,
        idempotent: true,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::begin_control_session,
    },
    ToolSpec {
        name: END_CONTROL_SESSION,
        title: "End control session",
        route: Route::EndSession,
        idempotent: true,
        target_ref: TargetRef::None,
        reads_screen: false,
        schema: schema::end_control_session,
    },
    ToolSpec {
        name: LIST_DISPLAYS,
        title: "List displays",
        route: Route::Observe(handler!(call_list_displays)),
        idempotent: true,
        target_ref: TargetRef::None,
        reads_screen: false,
        schema: schema::list_displays,
    },
    ToolSpec {
        name: CAPTURE_DISPLAY,
        title: "Capture display",
        route: Route::Observe(handler!(call_capture_display)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::capture_display,
    },
    ToolSpec {
        name: CAPTURE_REGION,
        title: "Capture region",
        route: Route::Observe(handler!(call_capture_region)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::capture_region,
    },
    ToolSpec {
        name: WAIT_FOR_VISUAL_CHANGE,
        title: "Wait for visual change",
        route: Route::Watch(handler!(call_wait_for_visual_change)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::wait_for_visual_change,
    },
    ToolSpec {
        name: CAPTURE_VISUAL_BASELINE,
        title: "Save visual baseline",
        route: Route::Observe(handler!(call_capture_visual_baseline)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::capture_visual_baseline,
    },
    ToolSpec {
        name: WAIT_FOR_CHANGE_SINCE,
        title: "Wait for change since baseline",
        route: Route::Watch(handler!(call_wait_for_change_since)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::wait_for_change_since,
    },
    ToolSpec {
        name: READ_TEXT_IN_REGION,
        title: "Read text (OCR)",
        route: Route::Watch(handler!(call_read_text_in_region)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::read_text_in_region,
    },
    ToolSpec {
        name: FIND_TEXT_ON_SCREEN,
        title: "Find text (OCR)",
        route: Route::Watch(handler!(call_find_text_on_screen)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::find_text_on_screen,
    },
    ToolSpec {
        name: WAIT_FOR_TEXT,
        title: "Wait for text (OCR)",
        route: Route::Watch(handler!(call_wait_for_text)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::wait_for_text,
    },
    ToolSpec {
        name: CLICK_TEXT,
        title: "Click text (OCR)",
        route: Route::Input(prepare_click_text),
        idempotent: false,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::click_text,
    },
    ToolSpec {
        name: LIST_VIRTUAL_DESKTOPS,
        title: "List virtual desktops",
        route: Route::Observe(handler!(call_list_virtual_desktops)),
        idempotent: true,
        target_ref: TargetRef::None,
        reads_screen: false,
        schema: schema::list_virtual_desktops,
    },
    ToolSpec {
        name: SWITCH_VIRTUAL_DESKTOP,
        title: "Switch virtual desktop",
        route: Route::Input(prepare_switch_virtual_desktop),
        idempotent: false,
        target_ref: TargetRef::Optional,
        reads_screen: false,
        schema: schema::switch_virtual_desktop,
    },
    ToolSpec {
        name: LIST_WINDOWS,
        title: "List windows",
        route: Route::Observe(handler!(call_list_windows)),
        idempotent: true,
        target_ref: TargetRef::None,
        reads_screen: false,
        schema: schema::list_windows,
    },
    ToolSpec {
        name: FOCUS_WINDOW,
        title: "Focus window",
        route: Route::Input(prepare_focus_window),
        idempotent: true,
        target_ref: TargetRef::None,
        reads_screen: false,
        schema: schema::focus_window,
    },
    ToolSpec {
        name: CAPTURE_WINDOW,
        title: "Capture window",
        route: Route::Observe(handler!(call_capture_window)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: true,
        schema: schema::capture_window,
    },
    ToolSpec {
        name: FIND_ELEMENTS,
        title: "Find elements",
        // A read-only observation: it claims no session and sends no input. It can wait for a
        // slow provider for seconds, so a stop cancels it like the other waits.
        route: Route::Watch(handler!(call_find_elements)),
        idempotent: false,
        target_ref: TargetRef::Required,
        // It reads the window's content, so the indicator shows the reading state as for a
        // screenshot.
        reads_screen: true,
        schema: schema::find_elements,
    },
    ToolSpec {
        name: WAIT_FOR_WINDOW,
        title: "Wait for window",
        route: Route::Watch(handler!(call_wait_for_window)),
        idempotent: false,
        target_ref: TargetRef::None,
        reads_screen: false,
        schema: schema::wait_for_window,
    },
    ToolSpec {
        name: MOVE_MOUSE,
        title: "Move mouse",
        route: Route::Input(prepare_move_mouse),
        idempotent: true,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::move_mouse,
    },
    ToolSpec {
        name: CLICK_MOUSE,
        title: "Click mouse",
        route: Route::Input(prepare_click_mouse),
        idempotent: false,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::click_mouse,
    },
    ToolSpec {
        name: DRAG_MOUSE,
        title: "Drag mouse",
        route: Route::Input(prepare_drag_mouse),
        idempotent: false,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::drag_mouse,
    },
    ToolSpec {
        name: SCROLL_MOUSE,
        title: "Scroll mouse",
        route: Route::Input(prepare_scroll_mouse),
        idempotent: false,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::scroll_mouse,
    },
    ToolSpec {
        name: PRESS_KEYS,
        title: "Press keys",
        route: Route::Input(prepare_press_keys),
        idempotent: false,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::press_keys,
    },
    ToolSpec {
        name: TYPE_TEXT,
        title: "Type text",
        route: Route::Input(prepare_type_text),
        idempotent: false,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::type_text,
    },
    ToolSpec {
        name: RUN_ACTIONS,
        title: "Run action batch",
        route: Route::Input(prepare_run_actions),
        idempotent: false,
        target_ref: TargetRef::Required,
        reads_screen: false,
        schema: schema::run_actions,
    },
    ToolSpec {
        name: STOP_DESKTOP_WORK,
        title: "Emergency stop",
        route: Route::Stop,
        idempotent: true,
        target_ref: TargetRef::None,
        reads_screen: false,
        schema: schema::stop_desktop_work,
    },
];

/// The catalog row of a tool, if the tool exists.
pub(super) fn spec(tool: &str) -> Option<&'static ToolSpec> {
    CATALOG.iter().find(|spec| spec.name == tool)
}
