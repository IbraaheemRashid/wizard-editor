use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

pub fn handle_keyboard(ctx: &egui::Context, state: &mut AppState) {
    ctx.input(|i| {
        if i.key_pressed(egui::Key::L) {
            match state.project.playback.state {
                PlaybackState::Playing => {
                    state.project.playback.speed = (state.project.playback.speed * 2.0).min(4.0);
                }
                _ => {
                    state.project.playback.speed = 1.0;
                    state.project.playback.state = PlaybackState::Playing;
                }
            }
        }
        if i.key_pressed(egui::Key::J) {
            match state.project.playback.state {
                PlaybackState::PlayingReverse => {
                    state.project.playback.speed = (state.project.playback.speed * 2.0).min(4.0);
                }
                _ => {
                    state.project.playback.speed = 1.0;
                    state.project.playback.play_reverse();
                }
            }
        }
        if i.key_pressed(egui::Key::K) {
            state.project.playback.speed = 1.0;
            state.project.playback.stop();
        }
        if i.key_pressed(egui::Key::Space) {
            state.project.playback.speed = 1.0;
            state.project.playback.toggle_play();
        }
    });
}
