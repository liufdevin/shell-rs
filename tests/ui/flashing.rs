use crate::support::*;
use shellrs::app::{CloseFlashing, OpenFlashing};

#[gpui_kit::test]
fn opening_the_flashing_tab_shows_the_firmware_picker_and_log(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, _workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenFlashing), cx)
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _cx| {
        assert!(window.try_find("flashing").is_some());
        assert!(window.try_find("flashing-choose").is_some());
        assert!(window.try_find("flashing-log").is_some());
    });
}

#[gpui_kit::test]
fn closing_the_flashing_tab_removes_it(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, _workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenFlashing), cx)
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseFlashing), cx)
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _cx| {
        assert!(window.try_find("flashing").is_none());
    });
}
