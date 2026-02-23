use std::path::PathBuf;

use wizard_media::pipeline::{
    AudioOnlyHandle, DecodedFrame, PipelineHandle, ReversePipelineHandle,
};
use wizard_state::clip::ClipId;
use wizard_state::timeline::TimelineClipId;

use crate::constants::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PipelineStatus {
    StartingUp,
    Delivering,
    Stalled,
    LongStall,
}

impl PipelineStatus {
    pub fn is_stalled(self) -> bool {
        matches!(self, PipelineStatus::Stalled | PipelineStatus::LongStall)
    }
}

pub struct ForwardPipelineState {
    pub handle: PipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub pts_offset: Option<f64>,
    pub speed: f64,
    pub frame_delivered: bool,
    pub started_at: f64,
    pub last_frame_time: Option<f64>,
    pub age: u32,
}

impl ForwardPipelineState {
    pub fn status(&self, now: f64) -> PipelineStatus {
        if !self.frame_delivered {
            return if now - self.started_at <= FORWARD_STARTUP_GRACE_S {
                PipelineStatus::StartingUp
            } else if now - self.started_at <= FORWARD_STARTUP_LONG_GRACE_S {
                PipelineStatus::Stalled
            } else {
                PipelineStatus::LongStall
            };
        }
        match self.last_frame_time {
            Some(t) if now - t > FRAME_GAP_LONG_STALL_S => PipelineStatus::LongStall,
            Some(t) if now - t > FRAME_GAP_STALL_S => PipelineStatus::Stalled,
            _ => PipelineStatus::Delivering,
        }
    }

    pub fn stall_status(&self, now: f64) -> PipelineStatus {
        if !self.frame_delivered {
            return if now - self.started_at <= FORWARD_STARTUP_GRACE_S {
                PipelineStatus::StartingUp
            } else {
                PipelineStatus::Stalled
            };
        }
        match self.last_frame_time {
            Some(t) if now - t > PIPELINE_STALL_THRESHOLD_S => PipelineStatus::Stalled,
            _ => PipelineStatus::Delivering,
        }
    }
}

pub struct ShadowPipelineState {
    pub handle: PipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub first_frame_ready: bool,
    pub buffered_frame: Option<DecodedFrame>,
    pub audio_sources: Vec<(AudioOnlyHandle, wizard_audio::output::AudioConsumer)>,
}

pub struct ReversePipelineState {
    pub handle: ReversePipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub pts_offset: Option<f64>,
    pub speed: f64,
    pub started_at: f64,
    pub last_frame_time: Option<f64>,
}

impl ReversePipelineState {
    pub fn status(&self, now: f64) -> PipelineStatus {
        match self.last_frame_time {
            None => {
                if now - self.started_at <= FORWARD_STARTUP_GRACE_S {
                    PipelineStatus::StartingUp
                } else if now - self.started_at <= FORWARD_STARTUP_LONG_GRACE_S {
                    PipelineStatus::Stalled
                } else {
                    PipelineStatus::LongStall
                }
            }
            Some(t) if now - t > FRAME_GAP_LONG_STALL_S => PipelineStatus::LongStall,
            Some(t) if now - t > FRAME_GAP_STALL_S => PipelineStatus::Stalled,
            _ => PipelineStatus::Delivering,
        }
    }

    pub fn stall_status(&self, now: f64) -> PipelineStatus {
        match self.last_frame_time {
            None => {
                if now - self.started_at <= FORWARD_STARTUP_GRACE_S {
                    PipelineStatus::StartingUp
                } else {
                    PipelineStatus::Stalled
                }
            }
            Some(t) if now - t > PIPELINE_STALL_THRESHOLD_S => PipelineStatus::Stalled,
            _ => PipelineStatus::Delivering,
        }
    }
}
