use super::{EntityBase, NBTStorage, player::Player};
use async_trait::async_trait;
use crossbeam::atomic::AtomicCell;
use pumpkin_data::damage::DamageType;
use pumpkin_nbt::compound::NbtCompound;

// TODO: This entire thing should be atomic, not individual fields
pub struct HungerManager {
    /// The current hunger level.
    pub level: AtomicCell<u8>,
    /// The food saturation level.
    pub saturation: AtomicCell<f32>,
    pub exhaustion: AtomicCell<f32>,
    pub tick_timer: AtomicCell<u32>,
}

impl Default for HungerManager {
    fn default() -> Self {
        Self {
            level: AtomicCell::new(20),
            saturation: AtomicCell::new(5.0),
            exhaustion: AtomicCell::new(0.0),
            tick_timer: AtomicCell::new(0),
        }
    }
}

impl HungerManager {
    pub async fn tick(&self, player: &Player) {
        let saturation = self.saturation.load();
        let level = self.level.load();
        let exhaustion = self.exhaustion.load();
        // Decrease hunger level on exhaustion
        if level != 0 && exhaustion > 4.0 {
            self.exhaustion.store(exhaustion - 4.0);
            if saturation > 0.0 {
                self.saturation.store((saturation - 1.0).max(0.0));
            } else {
                self.level.store(level - 1);
                player.send_health().await;
            }
        }
        // Heal when hunger is full
        if saturation > 0.0 && player.can_food_heal() && level >= 20 {
            self.tick_timer.fetch_add(1);
            if self.tick_timer.load() >= 10 {
                let saturation = saturation.min(6.0);
                player.heal(saturation / 6.0).await;
                self.add_exhausten(saturation);
                self.tick_timer.store(0);
            }
        } else if level == 0 {
            self.tick_timer.fetch_add(1);
            if self.tick_timer.load() >= 80 {
                player.damage(1.0, DamageType::STARVE, None).await;
                self.tick_timer.store(0);
            }
        } else {
            self.tick_timer.store(0);
        }
    }

    pub fn add_exhausten(&self, exhaustion: f32) {
        self.exhaustion
            .store((self.exhaustion.load() + exhaustion).min(40.0));
    }
}

#[async_trait]
impl NBTStorage for HungerManager {
    // TODO: Proper value checks

    async fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_int("foodLevel", self.level.load().into());
        nbt.put_float("foodSaturationLevel", self.saturation.load());
        nbt.put_float("foodExhaustionLevel", self.exhaustion.load());
        nbt.put_int("foodTickTimer", self.tick_timer.load() as i32);
    }

    async fn read_nbt(&mut self, nbt: &mut NbtCompound) {
        self.level
            .store(nbt.get_int("foodLevel").unwrap_or(20) as u8);
        self.saturation
            .store(nbt.get_float("foodSaturationLevel").unwrap_or(5.0));
        self.exhaustion
            .store(nbt.get_float("foodExhaustionLevel").unwrap_or(0.0));
        self.tick_timer
            .store(nbt.get_int("foodTickTimer").unwrap_or(0) as u32);
    }
}
