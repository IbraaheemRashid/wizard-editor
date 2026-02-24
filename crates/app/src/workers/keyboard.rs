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
        if i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace) {
            if !state.ui.selection.selected_timeline_clips.is_empty() {
                state.project.snapshot_for_undo();
                let to_delete: Vec<_> =
                    state.ui.selection.selected_timeline_clips.drain().collect();
                for clip_id in to_delete {
                    state.project.timeline.remove_clip(clip_id);
                }
            }
        }
        if i.modifiers.command && !i.modifiers.shift && i.key_pressed(egui::Key::Z) {
            state.project.undo();
        }
        if i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::Z) {
            state.project.redo();
        }
    });
}
