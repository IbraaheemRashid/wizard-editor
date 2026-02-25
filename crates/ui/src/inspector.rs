use wizard_state::clip::ClipId;
use wizard_state::project::AppState;
use wizard_state::tag::Tag;
use wizard_state::timeline::TrackKind;

use crate::theme;

// FOURTH PANEL
pub fn inspector_panel(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("Inspector");
    ui.separator();
    show_project_summary(ui, state);
    ui.separator();

    let selected_timeline_clip = state.ui.selection.primary_timeline_clip();
    let selected_clip = selected_clip_id(state, selected_timeline_clip);

    let Some(selected_clip) = selected_clip else {
        ui.colored_label(theme::TEXT_DIM, "Select a browser or timeline clip");
        return;
    };

    let (
        display_name,
        filename,
        path,
        duration,
        resolution,
        codec,
        audio_only,
        is_starred,
        tag_mask,
    ) = match state.project.clips.get(&selected_clip) {
        Some(clip) => (
            clip.display_name().to_string(),
            clip.filename.clone(),
            clip.path.display().to_string(),
            clip.duration,
            clip.resolution,
            clip.codec.clone().unwrap_or_else(|| "Unknown".to_string()),
            clip.audio_only,
            state.project.starred.contains(&selected_clip),
            state.project.clip_tag_mask(selected_clip),
        ),
        None => {
            ui.colored_label(theme::TEXT_DIM, "Selected clip is no longer available");
            return;
        }
    };

    ui.label(egui::RichText::new(display_name).strong());
    ui.colored_label(theme::TEXT_DIM, filename);
    ui.colored_label(theme::TEXT_DIM, path);
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        if ui
            .button(if is_starred { "Unstar" } else { "Star" })
            .clicked()
        {
            state.project.toggle_star(selected_clip);
        }
        if ui.button("Select In Browser").clicked() {
            state.ui.selection.select_single(selected_clip);
        }
    });

    ui.add_space(6.0);
    ui.label("Tags");
    ui.horizontal_wrapped(|ui| {
        for tag in Tag::ALL {
            let has_tag = (tag_mask & tag.bit()) != 0;
            if ui.selectable_label(has_tag, tag.label()).clicked() {
                state.project.toggle_tag(selected_clip, tag);
                rebuild_clip_search_haystack(state, selected_clip);
            }
        }
    });

    ui.separator();
    ui.label("Clip Metadata");
    ui.colored_label(
        theme::TEXT_DIM,
        match duration {
            Some(seconds) => format!("Duration: {:.2}s", seconds),
            None => "Duration: Unknown".to_string(),
        },
    );
    ui.colored_label(
        theme::TEXT_DIM,
        match resolution {
            Some((w, h)) => format!("Resolution: {w}x{h}"),
            None => "Resolution: Unknown".to_string(),
        },
    );
    ui.colored_label(theme::TEXT_DIM, format!("Codec: {codec}"));
    ui.colored_label(
        theme::TEXT_DIM,
        format!(
            "Type: {}",
            if audio_only {
                "Audio only"
            } else {
                "Video + Audio"
            }
        ),
    );

    if let Some(timeline_clip_id) = selected_timeline_clip {
        ui.separator();
        if let Some((track, _, timeline_clip)) = state.project.timeline.find_clip(timeline_clip_id)
        {
            ui.label("Timeline Instance");
            ui.colored_label(
                theme::TEXT_DIM,
                format!(
                    "Track: {} ({})",
                    track.name,
                    match track.kind {
                        TrackKind::Video => "video",
                        TrackKind::Audio => "audio",
                    }
                ),
            );
            ui.colored_label(
                theme::TEXT_DIM,
                format!("Start: {:.2}s", timeline_clip.timeline_start),
            );
            ui.colored_label(
                theme::TEXT_DIM,
                format!("Duration: {:.2}s", timeline_clip.duration),
            );
            ui.colored_label(
                theme::TEXT_DIM,
                format!(
                    "Source range: {:.2}s -> {:.2}s",
                    timeline_clip.source_in, timeline_clip.source_out
                ),
            );
            if ui.button("Jump Playhead To Clip Start").clicked() {
                state.project.playback.playhead = timeline_clip.timeline_start;
            }
        }
    }
}

fn selected_clip_id(
    state: &AppState,
    selected_timeline_clip: Option<wizard_state::timeline::TimelineClipId>,
) -> Option<ClipId> {
    if let Some(clip_id) = state.ui.selection.primary_clip() {
        return Some(clip_id);
    }
    selected_timeline_clip.and_then(|timeline_clip_id| {
        state
            .project
            .timeline
            .find_clip(timeline_clip_id)
            .map(|(_, _, timeline_clip)| timeline_clip.source_id)
    })
}

fn rebuild_clip_search_haystack(state: &mut AppState, clip_id: ClipId) {
    let tag_mask = state.project.clip_tag_mask(clip_id);
    if let Some(clip) = state.project.clips.get_mut(&clip_id) {
        clip.rebuild_search_haystack(tag_mask);
    }
}

fn show_project_summary(ui: &mut egui::Ui, state: &AppState) {
    ui.label("Project");
    ui.colored_label(
        theme::TEXT_DIM,
        format!("Clips: {}", state.project.clips.len()),
    );
    ui.colored_label(
        theme::TEXT_DIM,
        format!(
            "Tracks: {} video / {} audio",
            state.project.timeline.video_tracks.len(),
            state.project.timeline.audio_tracks.len()
        ),
    );
    ui.colored_label(
        theme::TEXT_DIM,
        format!("Timeline clips: {}", timeline_clip_count(state)),
    );
}

fn timeline_clip_count(state: &AppState) -> usize {
    state
        .project
        .timeline
        .video_tracks
        .iter()
        .chain(state.project.timeline.audio_tracks.iter())
        .map(|track| track.clips.len())
        .sum()
}
