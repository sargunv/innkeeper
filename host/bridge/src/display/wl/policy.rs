//! Window policy without Wayland objects or side effects. Shape: 0 unchanged, 1 portrait, 2 landscape.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AppRequest {
    pub immersive: bool,
    pub fullscreen: bool,
    pub shape: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Policy {
    app: AppRequest,
    // A windowed choice expires on immersive entry; user-entered fullscreen lasts until exit.
    user_override: Option<bool>,
    pending_shape: u32,
    // Last fullscreen request until acknowledged; other configures may belong to older requests.
    requested_fullscreen: Option<bool>,
}

#[derive(Clone, Copy)]
pub(super) enum Event {
    App(AppRequest),
    ToggleFullscreen,
    Rotate,
    Configure { was_fullscreen: bool },
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
    let previous_fullscreen = policy.user_override.unwrap_or(policy.app.fullscreen);
    match event {
        Event::App(app) => {
            if app.immersive && !policy.app.immersive && policy.user_override == Some(false) {
                policy.user_override = None;
            }
            if app.shape != policy.app.shape {
                policy.pending_shape = app.shape;
            }
            policy.app = app;
        }
        Event::ToggleFullscreen => policy.user_override = Some(!compositor.fullscreen),
        Event::Rotate if !compositor.fullscreen && !compositor.constrained => {
            policy.pending_shape = 3 - compositor.shape;
        }
        Event::Configure { was_fullscreen } => {
            if let Some(requested) = policy.requested_fullscreen {
                if compositor.fullscreen == requested {
                    policy.requested_fullscreen = None;
                }
            } else if compositor.fullscreen != was_fullscreen
                && compositor.fullscreen != previous_fullscreen
            {
                policy.user_override = Some(compositor.fullscreen);
            }
        }
        _ => {}
    }
    let fullscreen = policy.user_override.unwrap_or(policy.app.fullscreen);
    let request = (fullscreen != previous_fullscreen
        && (policy.requested_fullscreen.is_some() || fullscreen != compositor.fullscreen))
        .then_some(fullscreen);
    if request.is_some() {
        policy.requested_fullscreen = request;
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

    fn app(immersive: bool, fullscreen: bool, shape: u32) -> Event {
        Event::App(AppRequest {
            immersive,
            fullscreen,
            shape,
        })
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
        let was_windowed = Configure {
            was_fullscreen: false,
        };
        let was_fullscreen = Configure {
            was_fullscreen: true,
        };
        let cases: &[(&str, &[Step])] = &[
            (
                "fullscreen reversal before acknowledgement",
                &[
                    (app(true, true, 2), land, (true, 0, Some(true))),
                    (app(false, false, 2), land, (false, 2, Some(false))),
                    (was_windowed, full, (false, 0, None)),
                    (was_fullscreen, land, (false, 0, None)),
                    (app(true, true, 2), land, (true, 0, Some(true))),
                ],
            ),
            (
                "normal immersive app offers fullscreen",
                &[
                    (app(true, false, 0), land, (false, 0, None)),
                    (ToggleFullscreen, land, (true, 0, Some(true))),
                    (was_windowed, full, (true, 0, None)),
                    (app(false, false, 0), full, (true, 0, None)),
                    (app(true, false, 0), full, (true, 0, None)),
                ],
            ),
            (
                "immersive landscape game and automatic restoration",
                &[
                    (app(true, true, 2), port, (true, 0, Some(true))),
                    (was_windowed, full, (true, 0, None)),
                    (Rotate, full, (true, 0, None)),
                    (app(false, false, 2), full, (false, 0, Some(false))),
                    (was_fullscreen, port, (false, 2, None)),
                    (app(true, true, 2), land, (true, 0, Some(true))),
                ],
            ),
            (
                "immersive sensor game",
                &[(app(true, true, 0), land, (true, 0, Some(true)))],
            ),
            (
                "maximized defers app orientation and ignores Rotate",
                &[
                    (app(false, false, 1), max, (false, 0, None)),
                    (Rotate, max, (false, 0, None)),
                    (was_windowed, land, (false, 1, None)),
                ],
            ),
            (
                "tiled defers app orientation and ignores Rotate",
                &[
                    (app(false, false, 2), tiled, (false, 0, None)),
                    (Rotate, tiled, (false, 0, None)),
                    (was_windowed, port, (false, 2, None)),
                ],
            ),
            (
                "returning activity preserves its reported runtime orientation",
                &[
                    (app(false, false, 1), port, (false, 1, None)),
                    (app(false, false, 2), port, (false, 2, None)),
                    (app(false, false, 1), land, (false, 1, None)),
                    (app(false, false, 2), port, (false, 2, None)),
                ],
            ),
            (
                "F11 exit persists across orientation eligibility changes",
                &[
                    (app(true, true, 2), land, (true, 0, Some(true))),
                    (was_windowed, full, (true, 0, None)),
                    (ToggleFullscreen, full, (false, 0, Some(false))),
                    (was_fullscreen, land, (false, 2, None)),
                    (app(true, false, 1), land, (false, 1, None)),
                    (app(true, true, 2), port, (false, 2, None)),
                    (app(false, false, 2), land, (false, 0, None)),
                    (app(true, true, 2), land, (true, 0, Some(true))),
                ],
            ),
            (
                "native exit from app-entered fullscreen persists",
                &[
                    (app(true, true, 2), land, (true, 0, Some(true))),
                    (was_windowed, full, (true, 0, None)),
                    (was_fullscreen, land, (false, 2, None)),
                    (app(true, false, 1), land, (false, 1, None)),
                    (app(true, true, 2), port, (false, 2, None)),
                ],
            ),
            (
                "native exit from user-entered fullscreen persists",
                &[
                    (app(true, false, 1), port, (false, 1, None)),
                    (was_windowed, full, (true, 0, None)),
                    (was_fullscreen, port, (false, 0, None)),
                    (app(true, true, 2), port, (false, 2, None)),
                    (app(false, false, 2), land, (false, 0, None)),
                    (app(true, true, 2), land, (true, 0, Some(true))),
                ],
            ),
            (
                "Rotate survives repeated app metadata and configure",
                &[
                    (app(false, false, 2), land, (false, 2, None)),
                    (Rotate, land, (false, 1, None)),
                    (was_windowed, port, (false, 0, None)),
                    (app(false, false, 2), port, (false, 0, None)),
                    (app(false, false, 1), port, (false, 1, None)),
                    (app(false, false, 2), port, (false, 2, None)),
                ],
            ),
            (
                "compositor may refuse the requested shape",
                &[
                    (app(false, false, 1), land, (false, 1, None)),
                    (was_windowed, land, (false, 0, None)),
                    (app(false, false, 1), land, (false, 0, None)),
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
