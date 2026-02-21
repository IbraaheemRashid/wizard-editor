#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    PlayingReverse,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self::Stopped
    }
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
    pub fn toggle_play(&mut self) {
        self.state = match self.state {
            PlaybackState::Playing => PlaybackState::Stopped,
            _ => PlaybackState::Playing,
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

    pub fn advance(&mut self, dt: f64) {
        match self.state {
            PlaybackState::Playing => self.playhead += dt * self.speed,
            PlaybackState::PlayingReverse => self.playhead -= dt * self.speed,
            PlaybackState::Stopped => {}
        }
        if self.playhead < 0.0 {
            self.playhead = 0.0;
            self.state = PlaybackState::Stopped;
        }
    }
}
