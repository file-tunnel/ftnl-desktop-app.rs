use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const OPEN_ID: &str = "ftnl.open";
const HIDE_ID: &str = "ftnl.hide";
const PAUSE_ID: &str = "ftnl.pause";
const RESUME_ID: &str = "ftnl.resume";
const QUIT_ID: &str = "ftnl.quit";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellAction {
    OpenWindow,
    HideWindow,
    PauseCapture,
    ResumeCapture,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellState {
    pub window_visible: bool,
    pub capture_active: bool,
    pub quit_requested: bool,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            window_visible: true,
            capture_active: false,
            quit_requested: false,
        }
    }
}

impl ShellState {
    pub fn apply(&mut self, action: ShellAction) {
        match action {
            ShellAction::OpenWindow => self.window_visible = true,
            ShellAction::HideWindow => self.window_visible = false,
            ShellAction::PauseCapture => self.capture_active = false,
            ShellAction::ResumeCapture => self.capture_active = true,
            ShellAction::Quit => self.quit_requested = true,
        }
    }

    pub fn on_close_requested(&mut self, tray_available: bool) -> bool {
        if self.quit_requested || !tray_available {
            false
        } else {
            self.window_visible = false;
            true
        }
    }
}

pub struct NativeTray {
    _tray: TrayIcon,
}

impl NativeTray {
    pub fn new() -> Result<Self, ()> {
        let menu = Menu::new();
        let open = MenuItem::with_id(OPEN_ID, "Open File Tunnel", true, None);
        let hide = MenuItem::with_id(HIDE_ID, "Hide window", true, None);
        let pause = MenuItem::with_id(PAUSE_ID, "Pause clipboard capture", true, None);
        let resume = MenuItem::with_id(RESUME_ID, "Resume clipboard capture", true, None);
        let quit = MenuItem::with_id(QUIT_ID, "Quit File Tunnel", true, None);
        menu.append_items(&[&open, &hide, &pause, &resume, &quit])
            .map_err(|_| ())?;

        let tray = TrayIconBuilder::new()
            .with_id("ftnl.desktop")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("File Tunnel")
            .with_icon(app_icon()?)
            .build()
            .map_err(|_| ())?;

        Ok(Self { _tray: tray })
    }

    pub fn poll_actions(&self) -> Vec<ShellAction> {
        let mut actions = MenuEvent::receiver()
            .try_iter()
            .filter_map(|event| menu_action(event.id.as_ref()))
            .collect::<Vec<_>>();

        actions.extend(
            TrayIconEvent::receiver()
                .try_iter()
                .filter_map(|event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => Some(ShellAction::OpenWindow),
                    _ => None,
                }),
        );
        actions
    }
}

fn menu_action(id: &str) -> Option<ShellAction> {
    match id {
        OPEN_ID => Some(ShellAction::OpenWindow),
        HIDE_ID => Some(ShellAction::HideWindow),
        PAUSE_ID => Some(ShellAction::PauseCapture),
        RESUME_ID => Some(ShellAction::ResumeCapture),
        QUIT_ID => Some(ShellAction::Quit),
        _ => None,
    }
}

fn app_icon() -> Result<Icon, ()> {
    const SIDE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let inside = (5..27).contains(&x) && (5..27).contains(&y);
            let tunnel = (12..20).contains(&x) || (12..20).contains(&y);
            let (red, green, blue, alpha) = if inside && tunnel {
                (238, 246, 255, 255)
            } else if inside {
                (29, 99, 237, 255)
            } else {
                (0, 0, 0, 0)
            };
            rgba.extend_from_slice(&[red, green, blue, alpha]);
        }
    }
    Icon::from_rgba(rgba, SIDE, SIDE).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_hides_until_an_explicit_quit() {
        let mut state = ShellState::default();
        assert!(state.on_close_requested(true));
        assert!(!state.window_visible);

        state.apply(ShellAction::OpenWindow);
        state.apply(ShellAction::Quit);
        assert!(!state.on_close_requested(true));
        assert!(state.window_visible);

        let mut unavailable = ShellState::default();
        assert!(!unavailable.on_close_requested(false));
        assert!(unavailable.window_visible);
    }

    #[test]
    fn capture_and_window_transitions_are_independent() {
        let mut state = ShellState::default();
        state.apply(ShellAction::ResumeCapture);
        state.apply(ShellAction::HideWindow);
        assert!(state.capture_active);
        assert!(!state.window_visible);

        state.apply(ShellAction::PauseCapture);
        state.apply(ShellAction::OpenWindow);
        assert!(!state.capture_active);
        assert!(state.window_visible);
    }

    #[test]
    fn menu_ids_are_closed_and_unknown_ids_are_ignored() {
        assert_eq!(menu_action(OPEN_ID), Some(ShellAction::OpenWindow));
        assert_eq!(menu_action(PAUSE_ID), Some(ShellAction::PauseCapture));
        assert_eq!(menu_action("ftnl.future"), None);
    }

    #[test]
    fn generated_icon_has_the_expected_dimensions() {
        assert!(app_icon().is_ok());
    }
}
