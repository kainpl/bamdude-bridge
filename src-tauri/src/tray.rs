//! The tray icon, and the app's only way out.
//!
//! Closing the window hides it (see the close handler in [`crate::run`]), so
//! the tray is not decoration — it is the sole remaining route to both halves
//! of "where did my app go": bringing the window back, and actually quitting.
//! If this fails to build, the app becomes unquittable through the UI.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;

const MENU_OPEN: &str = "open";
const MENU_QUIT: &str = "quit";

pub fn build(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let open = MenuItem::with_id(app, MENU_OPEN, "Open BamDude Bridge", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or("the bundle carries no window icon to reuse for the tray")?;

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("BamDude Bridge")
        .menu(&menu)
        // Windows convention puts the menu on the right button. Leaving the
        // default on would make a left click open the menu as well, and then
        // the click-to-open-the-window handler below could never fire.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN => crate::show_settings(app),
            // ⚠️ The only real exit. Everything else hides.
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Act on release rather than press: a press that turns into a
            // drag is not a click, and reacting to the down edge makes the
            // window appear while the user is still moving the icon.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::show_settings(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}
