#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Capability {
    Search,
    TrackDetail,
    StreamUrl,
    Cover,
    Lyric,
    Playlist,
    Album,
    Artist,
}

#[derive(Debug, Clone, Default)]
pub struct CapabilitySet(std::collections::HashSet<Capability>);

impl CapabilitySet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, c: Capability) -> Self {
        self.0.insert(c);
        self
    }

    pub fn supports(&self, c: Capability) -> bool {
        self.0.contains(&c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_supports_nothing() {
        let s = CapabilitySet::new();
        assert!(!s.supports(Capability::Search));
    }

    #[test]
    fn builder_accumulates() {
        let s = CapabilitySet::new()
            .with(Capability::Search)
            .with(Capability::Lyric);
        assert!(s.supports(Capability::Search));
        assert!(s.supports(Capability::Lyric));
        assert!(!s.supports(Capability::StreamUrl));
    }
}
