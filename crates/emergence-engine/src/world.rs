/// A self-contained simulation, advanced one tick at a time by its host.
///
/// The host owns the clock: nothing happens between calls to [`World::tick`], which keeps the
/// simulation deterministic and lets a debugger pause or single-step it.
#[derive(Debug, Default)]
pub struct World {
    tick_count: u64,
}

impl World {
    /// Creates an empty world at tick zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advances the simulation by one tick.
    pub fn tick(&mut self) {
        self.tick_count += 1;
    }

    /// Returns how many ticks have run since the world was created.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }
}

#[cfg(test)]
mod tests {
    use super::World;

    #[test]
    fn new_world_starts_at_tick_zero() {
        assert_eq!(World::new().tick_count(), 0);
    }

    #[test]
    fn tick_advances_count() {
        let mut world = World::new();
        world.tick();
        world.tick();
        assert_eq!(world.tick_count(), 2);
    }
}
