use pumpkin_data::item::Item;
use pumpkin_data::tag::{RegistryKey, get_tag_values};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::text::TextComponent;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize};
use std::any::Any;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::option::Option;

mod categories;

#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
/// Item Rarity
pub enum Rarity {
    Common,
    UnCommon,
    Rare,
    Epic,
}

#[derive(Clone, Debug, Copy)]
pub struct ItemStack {
    pub item_count: u8,
    pub item: &'static Item,
}

impl Hash for ItemStack {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.item_count.hash(state);
        self.item.id.hash(state);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum ItemComponent {
    #[serde(rename = "minecraft:custom_data")]
    CustomData(NbtCompound),
    #[serde(rename = "minecraft:max_stack_size")]
    MaxStackSize(u8),
    #[serde(rename = "minecraft:max_damage")]
    MaxDamage(u32),
    #[serde(rename = "minecraft:damage")]
    Damage(u32),
    #[serde(rename = "minecraft:unbreakable")]
    Unbreakable,
    #[serde(rename = "minecraft:custom_name")]
    CustomName(TextComponent),
}

static COMPONENTS: std::sync::LazyLock<HashSet<&str>> = std::sync::LazyLock::new(|| {
    let mut set = HashSet::new();
    set.insert("minecraft:custom_data");
    set.insert("minecraft:max_stack_size");
    set.insert("minecraft:max_damage");
    set.insert("minecraft:damage");
    set.insert("minecraft:unbreakable");
    set.insert("minecraft:custom_name");
    set
});

pub trait ItemComponents {
    fn get_item_component(&self, key: &str) -> Option<&ItemComponent>;
}

#[derive(Clone)]
pub struct MapItemComponents {
    pub components: HashMap<&'static str, ItemComponent>,
}

impl ItemComponents for MapItemComponents {
    fn get_item_component(&self, key: &str) -> Option<&ItemComponent> {
        self.components.get(key)
    }
}

impl MapItemComponents {
    pub fn new() -> Self {
        Self {
            components: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct PatchedItemComponents {
    pub base: Cow<'static, MapItemComponents>,
    pub added: MapItemComponents,
    pub removed: HashSet<&'static str>,
}

impl ItemComponents for PatchedItemComponents {
    fn get_item_component(&self, key: &str) -> Option<&ItemComponent> {
        if self.removed.contains(key) {
            return None;
        }
        if let Some(value) = self.added.get_item_component(key) {
            return Some(value);
        }
        self.base.get_item_component(key)
    }
}

impl PatchedItemComponents {
    fn is_same_base(&self, other: &Self) -> bool {
        match (&self.base, &other.base) {
            (Cow::Borrowed(a_ref), Cow::Borrowed(b_ref)) => std::ptr::eq(
                *a_ref as *const MapItemComponents,
                *b_ref as *const MapItemComponents,
            ),
            (Cow::Owned(a_owned), Cow::Borrowed(b_ref)) => std::ptr::eq(
                a_owned as *const MapItemComponents,
                *b_ref as *const MapItemComponents,
            ),
            (Cow::Borrowed(a_ref), Cow::Owned(b_owned)) => std::ptr::eq(
                *a_ref as *const MapItemComponents,
                b_owned as *const MapItemComponents,
            ),
            _ => false,
        }
    }

    /// Returns a new instance of `PatchedItemComponents` with the base components.
    fn merge(&self, other: &Self) -> Result<Self, ()> {
        if Self::is_same_base(&self, &other) {
            // If the base components are the same, we can merge the added and removed components
            let mut merged_added = self.added.clone();
            for (key, value) in other.added.components.iter() {
                merged_added.components.insert(key, value.clone());
            }

            let mut merged_removed = self.removed.clone();
            for key in other.removed.iter() {
                if merged_added.components.contains_key(key) {
                    // If the key is in both added and removed, remove it from added
                    merged_added.components.remove(key);
                }
                merged_removed.insert(*key);
            }

            return Ok(Self {
                base: self.base.clone(),
                added: merged_added,
                removed: merged_removed,
            });
        }
        Err(())
    }
}

#[derive(Debug)]
pub struct ItemComponentPatch {
    pub removed: HashSet<&'static str>,
    pub patch: HashMap<&'static str, ItemComponent>,
}

impl Serialize for ItemComponentPatch {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_map(Some(self.removed.len() + self.patch.len()))?;
        for key in &self.removed {
            state.serialize_entry(
                &format!("!{}", key),
                &serde_json::Value::Object(serde_json::Map::new()),
            )?;
        }
        for (key, value) in &self.patch {
            let value = serde_json::to_value(value).map_err(|x| {
                serde::ser::Error::custom(format!("Failed to serialize item component {key}: {x}"))
            })?;
            if let serde_json::Value::Object(o) = value {
                for (k, v) in o {
                    state.serialize_entry(&k, &v)?;
                }
            } else {
                state.serialize_entry(key, &value)?;
            }
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for ItemComponentPatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ItemComponentPatchInner {
            patch: HashMap<String, serde_json::Value>,
            removed: HashSet<String>,
        }

        let map = serde_json::Map::deserialize(deserializer)?;

        let mut removed = HashSet::new();
        let mut patch = HashMap::new();
        for (key, value) in map {
            let mut map = serde_json::Map::new();
            if key.starts_with('!') {
                let key = key.strip_prefix('!').unwrap();
                if let Some(r) = COMPONENTS.get(key) {
                    removed.insert(*r);
                } else {
                    return Err(serde::de::Error::custom(format!(
                        "Unknown item component key: {key}"
                    )));
                }
            } else {
                map.insert(key.clone(), value);
                let component =
                    serde_json::from_value::<ItemComponent>(serde_json::Value::Object(map))
                        .map_err(|x| {
                            serde::de::Error::custom(format!(
                                "Failed to deserialize item component {key}: {x}"
                            ))
                        })?;
                if let Some(r) = COMPONENTS.get(key.as_str()) {
                    patch.insert(*r, component);
                } else {
                    return Err(serde::de::Error::custom(format!(
                        "Unknown item component key: {key}"
                    )));
                }
            }
        }

        Ok(ItemComponentPatch { removed, patch })
    }
}

/*
impl PartialEq for ItemStack {
    fn eq(&self, other: &Self) -> bool {
        self.item.id == other.item.id
    }
} */

impl ItemStack {
    pub const EMPTY: ItemStack = ItemStack {
        item_count: 0,
        item: &Item::AIR,
    };

    pub fn new(item_count: u8, item: &'static Item) -> Self {
        Self { item_count, item }
    }

    pub fn get_max_stack_size(&self) -> u8 {
        self.item.components.max_stack_size
    }

    pub fn get_item(&self) -> &Item {
        if self.is_empty() {
            &Item::AIR
        } else {
            self.item
        }
    }

    pub fn is_stackable(&self) -> bool {
        self.get_max_stack_size() > 1 // TODO: && (!this.isDamageable() || !this.isDamaged());
    }

    pub fn is_empty(&self) -> bool {
        self.item_count == 0 || self.item.id == Item::AIR.id
    }

    pub fn split(&mut self, amount: u8) -> Self {
        let min = amount.min(self.item_count);
        let stack = self.copy_with_count(min);
        self.decrement(min);
        stack
    }

    pub fn copy_with_count(&self, count: u8) -> Self {
        let mut stack = *self;
        stack.item_count = count;
        stack
    }

    pub fn set_count(&mut self, count: u8) {
        self.item_count = count;
    }

    pub fn decrement(&mut self, amount: u8) {
        self.item_count = self.item_count.saturating_sub(amount);
    }

    pub fn increment(&mut self, amount: u8) {
        self.item_count = self.item_count.saturating_add(amount);
    }

    pub fn are_items_and_components_equal(&self, other: &Self) -> bool {
        self.item == other.item //TODO: && self.item.components == other.item.components
    }

    pub fn are_equal(&self, other: &Self) -> bool {
        self.item_count == other.item_count && self.are_items_and_components_equal(other)
    }

    /// Determines the mining speed for a block based on tool rules.
    /// Direct matches return immediately, tagged blocks are checked separately.
    /// If no match is found, returns the tool's default mining speed or `1.0`.
    pub fn get_speed(&self, block: &str) -> f32 {
        // No tool? Use default speed
        let Some(tool) = &self.item.components.tool else {
            return 1.0;
        };

        for rule in tool.rules {
            // Skip if speed is not set
            let Some(speed) = rule.speed else {
                continue;
            };

            for entry in rule.blocks {
                if entry.eq(&block) {
                    return speed;
                }

                if entry.starts_with('#') {
                    // Check if block is in the tag group
                    if let Some(blocks) =
                        get_tag_values(RegistryKey::Block, entry.strip_prefix('#').unwrap())
                    {
                        if blocks.contains(&block) {
                            return speed;
                        }
                    }
                }
            }
        }
        // Return default mining speed if no match is found
        tool.default_mining_speed.unwrap_or(1.0)
    }

    /// Determines if a tool is valid for block drops based on tool rules.
    /// Direct matches return immediately, while tagged blocks are checked separately.
    pub fn is_correct_for_drops(&self, block: &str) -> bool {
        // Return false if no tool component exists
        let Some(tool) = &self.item.components.tool else {
            return false;
        };

        for rule in tool.rules {
            // Skip rules without a drop condition
            let Some(correct_for_drops) = rule.correct_for_drops else {
                continue;
            };

            for entry in rule.blocks {
                if entry.eq(&block) {
                    return correct_for_drops;
                }

                if entry.starts_with('#') {
                    // Check if block exists within the tag group
                    if let Some(blocks) =
                        get_tag_values(RegistryKey::Block, entry.strip_prefix('#').unwrap())
                    {
                        if blocks.contains(&block) {
                            return correct_for_drops;
                        }
                    }
                }
            }
        }
        false
    }

    pub fn write_item_stack(&self, compound: &mut NbtCompound) {
        // Minecraft 1.21.4 uses "id" as string with namespaced ID (minecraft:diamond_sword)
        compound.put_string("id", format!("minecraft:{}", self.item.registry_key));
        compound.put_int("count", self.item_count as i32);

        // Create a tag compound for additional data
        let tag = NbtCompound::new();

        // TODO: Store custom data like enchantments, display name, etc. would go here

        // Store custom data like enchantments, display name, etc. would go here
        compound.put_component("components", tag);
    }

    pub fn read_item_stack(compound: &NbtCompound) -> Option<Self> {
        // Get ID, which is a string like "minecraft:diamond_sword"
        let full_id = compound.get_string("id")?;

        // Remove the "minecraft:" prefix if present
        let registry_key = full_id.strip_prefix("minecraft:").unwrap_or(full_id);

        // Try to get item by registry key
        let item = Item::from_registry_key(registry_key)?;

        let count = compound.get_int("count")? as u8;

        // Create the item stack
        let item_stack = Self::new(count, item);

        // Process any additional data in the components compound
        if let Some(_tag) = compound.get_compound("components") {
            // TODO: Process additional components like damage, enchantments, etc.
        }

        Some(item_stack)
    }
}
