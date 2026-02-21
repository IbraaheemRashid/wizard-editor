#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tag {
    BRoll,
    VO,
    Music,
    Sfx,
}

impl Tag {
    pub const ALL: [Tag; 4] = [Tag::BRoll, Tag::VO, Tag::Music, Tag::Sfx];

    pub fn label(self) -> &'static str {
        match self {
            Tag::BRoll => "B-roll",
            Tag::VO => "VO",
            Tag::Music => "Music",
            Tag::Sfx => "SFX",
        }
    }

    pub fn bit(self) -> u32 {
        match self {
            Tag::BRoll => 1 << 0,
            Tag::VO => 1 << 1,
            Tag::Music => 1 << 2,
            Tag::Sfx => 1 << 3,
        }
    }
}
