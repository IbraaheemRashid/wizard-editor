#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing,
    PlayingReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Clone)]
pub struct Playback {
    pub state: PlaybackState,
    pub playhead: f64,
    pub speed: f64,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            state: PlaybackState::Stopped,
            playhead: 0.0,
            speed: 1.0,
        }
    }
}

impl Playback {
    pub fn direction(&self) -> PlaybackDirection {
        match self.state {
            PlaybackState::PlayingReverse => PlaybackDirection::Reverse,
            PlaybackState::Playing | PlaybackState::Stopped => PlaybackDirection::Forward,
        }
    }

    pub fn toggle_play(&mut self) {
        self.state = match self.state {
            PlaybackState::Playing => PlaybackState::Stopped,
            PlaybackState::PlayingReverse => PlaybackState::Stopped,
            PlaybackState::Stopped => PlaybackState::Playing,
        };
    }

    pub fn play_reverse(&mut self) {
        self.state = match self.state {
            PlaybackState::PlayingReverse => PlaybackState::Stopped,
            _ => PlaybackState::PlayingReverse,
        };
    }

    pub fn stop(&mut self) {
        self.state = PlaybackState::Stopped;
    }

    pub fn advance(&mut self, dt: f64, duration: f64) {
        match self.state {
            PlaybackState::Playing => self.playhead += dt * self.speed,
            PlaybackState::PlayingReverse => self.playhead -= dt * self.speed,
            PlaybackState::Stopped => {}
        }
        if self.playhead < 0.0 {
            self.playhead = 0.0;
            self.state = PlaybackState::Stopped;
        }
        if duration > 0.0 && self.playhead >= duration {
            self.playhead = duration;
            self.state = PlaybackState::Stopped;
        }
    }
}
