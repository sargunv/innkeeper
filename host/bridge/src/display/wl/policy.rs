//! Window policy without Wayland objects or side effects. Shape: 0 unchanged, 1 portrait, 2 landscape.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AppRequest {
    pub shape: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Policy {
    app: AppRequest,
    pending_shape: u32,
}

#[derive(Clone, Copy)]
pub(super) enum Event {
    App(AppRequest),
    ToggleFullscreen,
    Rotate,
    Configure,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct WindowState {
    pub fullscreen: bool,
    pub shape: u32,
    pub constrained: bool,
}

/// Reduce every app/user/compositor event to one desired state. Consume a shape request only
/// while floating: a later compositor configure wins, and Ctrl+R isn't undone by old app metadata.
pub(super) fn desired_window_state(
    mut policy: Policy,
    event: Event,
    compositor: WindowState,
) -> (Policy, WindowState, Option<bool>) {
    let mut fullscreen = compositor.fullscreen;
    let mut request = None;
    match event {
        Event::App(app) => {
            if app.shape != policy.app.shape {
                policy.pending_shape = app.shape;
            }
            policy.app = app;
        }
        Event::ToggleFullscreen => {
            fullscreen = !compositor.fullscreen;
            request = Some(fullscreen);
        }
        Event::Rotate if !compositor.fullscreen && !compositor.constrained => {
            policy.pending_shape = 3 - compositor.shape;
        }
        _ => {}
    }
    let shape = if fullscreen || compositor.fullscreen || compositor.constrained {
        0
    } else {
        std::mem::take(&mut policy.pending_shape)
    };
    let desired = WindowState {
        fullscreen,
        shape,
        ..compositor
    };
    (policy, desired, request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(shape: u32) -> Event {
        Event::App(AppRequest { shape })
    }

    fn window(fullscreen: bool, shape: u32, constrained: bool) -> WindowState {
        WindowState {
            fullscreen,
            shape,
            constrained,
        }
    }

    #[test]
    fn window_policy_cases() {
        use Event::{Configure, Rotate, ToggleFullscreen};
        // Each step supplies an event and the observed compositor state, then checks the
        // desired fullscreen/shape and any fullscreen command. No compositor is simulated.
        type Step = (Event, WindowState, (bool, u32, Option<bool>));
        let port = window(false, 1, false);
        let land = window(false, 2, false);
        let full = window(true, 2, false);
        let max = window(false, 2, true);
        let tiled = window(false, 1, true);
        let cases: &[(&str, &[Step])] = &[
            (
                "user fullscreen defers shape until exit",
                &[
                    (ToggleFullscreen, land, (true, 0, Some(true))),
                    (Configure, full, (true, 0, None)),
                    (app(1), full, (true, 0, None)),
                    (Rotate, full, (true, 0, None)),
                    (ToggleFullscreen, full, (false, 0, Some(false))),
                    (Configure, land, (false, 1, None)),
                    (app(1), port, (false, 0, None)),
                ],
            ),
            (
                "compositor fullscreen and exit persist across app metadata",
                &[
                    (Configure, full, (true, 0, None)),
                    (app(2), full, (true, 0, None)),
                    (Configure, land, (false, 2, None)),
                    (app(2), land, (false, 0, None)),
                    (app(1), land, (false, 1, None)),
                ],
            ),
            (
                "maximized defers app orientation and ignores Rotate",
                &[
                    (app(1), max, (false, 0, None)),
                    (Rotate, max, (false, 0, None)),
                    (Configure, land, (false, 1, None)),
                ],
            ),
            (
                "tiled defers app orientation and ignores Rotate",
                &[
                    (app(2), tiled, (false, 0, None)),
                    (Rotate, tiled, (false, 0, None)),
                    (Configure, port, (false, 2, None)),
                ],
            ),
            (
                "returning activity preserves its reported runtime orientation",
                &[
                    (app(1), port, (false, 1, None)),
                    (app(2), port, (false, 2, None)),
                    (app(1), land, (false, 1, None)),
                    (app(2), port, (false, 2, None)),
                ],
            ),
            (
                "Rotate survives repeated app metadata and configure",
                &[
                    (app(2), land, (false, 2, None)),
                    (Rotate, land, (false, 1, None)),
                    (Configure, port, (false, 0, None)),
                    (app(2), port, (false, 0, None)),
                    (app(1), port, (false, 1, None)),
                    (app(2), port, (false, 2, None)),
                ],
            ),
            (
                "compositor may refuse the requested shape",
                &[
                    (app(1), land, (false, 1, None)),
                    (Configure, land, (false, 0, None)),
                    (app(1), land, (false, 0, None)),
                ],
            ),
        ];
        for (name, steps) in cases {
            let mut policy = Policy::default();
            for (index, &(event, compositor, expected)) in steps.iter().enumerate() {
                let (next, desired, request) = desired_window_state(policy, event, compositor);
                assert_eq!(
                    (desired.fullscreen, desired.shape, request),
                    expected,
                    "{name}, step {index}",
                );
                policy = next;
            }
        }
    }
}
