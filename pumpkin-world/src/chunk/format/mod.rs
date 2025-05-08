use std::{
    collections::HashMap,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anvil::{WORLD_DATA_VERSION, chunk::SingleChunkDataSerializer};
use bytes::Bytes;
use pumpkin_data::{Block, chunk::ChunkStatus};
use pumpkin_nbt::{compound::NbtCompound, from_bytes, nbt_long_array};

use crate::{
    block::entities::block_entity_from_nbt, generation::section_coords, level::LevelFolder,
};
use pumpkin_util::math::{position::BlockPos, vector2::Vector2};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::{
    ChunkData, ChunkEntityData, ChunkHeightmaps, ChunkLightEngine, ChunkParsingError,
    ChunkReadingError, ChunkSections, ChunkSerializingError, ScheduledTick, SubChunk, TickPriority,
    io::{Dirtiable, file_manager::PathFromLevelFolder},
    palette::{BiomePalette, BlockPalette},
};

pub mod anvil;
pub mod linear;

// I can't use an tag because it will break ChunkNBT, but status need to have a big S, so "Status"
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct ChunkStatusWrapper {
    status: ChunkStatus,
}

async fn read_entire_file_to_bytes(path: &Path) -> Result<Bytes, ChunkReadingError> {
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(false)
        .create(false)
        .truncate(false)
        .open(path)
        .await
        .map_err(|err| match err.kind() {
            ErrorKind::NotFound => ChunkReadingError::ChunkNotExist,
            kind => ChunkReadingError::IoError(kind),
        })?;

    let capacity = match file.metadata().await {
        Ok(metadata) => metadata.len() as usize,
        Err(_) => 4096, // A sane default
    };

    // TODO: Memmap?
    let mut file_bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut file_bytes)
        .await
        .map_err(|err| ChunkReadingError::IoError(err.kind()))?;
    Ok(file_bytes.into())
}

impl PathFromLevelFolder for ChunkData {
    #[inline]
    fn file_path(folder: &LevelFolder, file_name: &str) -> PathBuf {
        folder.region_folder.join(file_name)
    }
}

impl Dirtiable for ChunkData {
    #[inline]
    fn mark_dirty(&mut self, flag: bool) {
        self.dirty = flag;
    }

    #[inline]
    fn is_dirty(&self) -> bool {
        self.dirty
    }
}

impl SingleChunkDataSerializer for ChunkData {
    #[inline]
    fn from_bytes(bytes: Bytes, pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
        Self::internal_from_bytes(&bytes, pos).map_err(ChunkReadingError::ParsingError)
    }

    #[inline]
    fn to_bytes(&self) -> Result<Bytes, ChunkSerializingError> {
        self.internal_to_bytes()
    }

    #[inline]
    fn position(&self) -> &Vector2<i32> {
        &self.position
    }
}

impl ChunkData {
    fn internal_from_bytes(
        chunk_data: &[u8],
        position: Vector2<i32>,
    ) -> Result<Self, ChunkParsingError> {
        // TODO: Implement chunk stages?
        if from_bytes::<ChunkStatusWrapper>(chunk_data)
            .map_err(ChunkParsingError::FailedReadStatus)?
            .status
            != ChunkStatus::Full
        {
            return Err(ChunkParsingError::ChunkNotGenerated);
        }

        let chunk_data = from_bytes::<ChunkNbt>(chunk_data)
            .map_err(|e| ChunkParsingError::ErrorDeserializingChunk(e.to_string()))?;

        if chunk_data.light_correct {
            for section in &chunk_data.sections {
                let mut block = false;
                let mut sky = false;
                let mut block_sum = 0;
                let mut sky_sum = 0;
                if let Some(block_light) = &section.block_light {
                    block = !block_light.is_empty();
                    block_sum = block_light
                        .iter()
                        .map(|b| ((*b >> 4) + (*b & 0x0F)) as usize)
                        .sum();
                }
                if let Some(sky_light) = &section.sky_light {
                    sky = !sky_light.is_empty();
                    sky_sum = sky_light
                        .iter()
                        .map(|b| ((*b >> 4) + (*b & 0x0F)) as usize)
                        .sum();
                }
                if (block || sky) && section.y == -5 {
                    log::trace!(
                        "section {},{},{}: block_light={}/{}, sky_light={}/{}",
                        chunk_data.x_pos,
                        section.y,
                        chunk_data.z_pos,
                        block,
                        block_sum,
                        sky,
                        sky_sum,
                    )
                }
            }
        }

        if chunk_data.x_pos != position.x || chunk_data.z_pos != position.z {
            return Err(ChunkParsingError::ErrorDeserializingChunk(format!(
                "Expected data for chunk {},{} but got it for {},{}!",
                position.x, position.z, chunk_data.x_pos, chunk_data.z_pos,
            )));
        }

        let light_engine = ChunkLightEngine {
            block_light: (0..chunk_data.sections.len() + 2)
                .map(|index| {
                    chunk_data
                        .sections
                        .iter()
                        .find(|section| {
                            section.y as i32 == index as i32 + chunk_data.min_y_section - 1
                        })
                        .and_then(|section| section.block_light.clone())
                        .map(LightContainer::new)
                        .unwrap_or_default()
                })
                .collect(),
            sky_light: (0..chunk_data.sections.len() + 2)
                .map(|index| {
                    chunk_data
                        .sections
                        .iter()
                        .find(|section| {
                            section.y as i32 == index as i32 + chunk_data.min_y_section - 1
                        })
                        .and_then(|section| section.sky_light.clone())
                        .map(LightContainer::new)
                        .unwrap_or_default()
                })
                .collect(),
        };
        let sub_chunks = chunk_data
            .sections
            .into_iter()
            .filter(|section| section.y >= chunk_data.min_y_section as i8)
            .map(|section| SubChunk {
                block_states: section
                    .block_states
                    .map(BlockPalette::from_disk_nbt)
                    .unwrap_or_default(),
                biomes: section
                    .biomes
                    .map(BiomePalette::from_disk_nbt)
                    .unwrap_or_default(),
            })
            .collect();
        let min_y = section_coords::section_to_block(chunk_data.min_y_section);
        let section = ChunkSections::new(sub_chunks, min_y);

        Ok(ChunkData {
            section,
            heightmap: chunk_data.heightmaps,
            position,
            // This chunk is read from disk, so it has not been modified
            dirty: false,
            block_ticks: chunk_data
                .block_ticks
                .iter()
                .map(|tick| ScheduledTick {
                    block_pos: BlockPos::new(tick.x, tick.y, tick.z),
                    delay: tick.delay as u16,
                    priority: TickPriority::from(tick.priority),
                    target_block_id: Block::from_registry_key(
                        &tick.target_block.replace("minecraft:", ""),
                    )
                    .unwrap_or(Block::AIR)
                    .id,
                })
                .collect(),
            fluid_ticks: chunk_data
                .fluid_ticks
                .iter()
                .map(|tick| ScheduledTick {
                    block_pos: BlockPos::new(tick.x, tick.y, tick.z),
                    delay: tick.delay as u16,
                    priority: TickPriority::from(tick.priority),
                    target_block_id: Block::from_registry_key(
                        &tick.target_block.replace("minecraft:", ""),
                    )
                    .unwrap_or(Block::AIR)
                    .id,
                })
                .collect(),
            block_entities: {
                let mut block_entities = HashMap::new();
                for nbt in chunk_data.block_entities {
                    let block_entity = block_entity_from_nbt(&nbt);
                    if let Some(block_entity) = block_entity {
                        block_entities.insert(block_entity.get_position(), block_entity);
                    }
                }
                block_entities
            },
            light_engine,
        })
    }

    fn internal_to_bytes(&self) -> Result<Bytes, ChunkSerializingError> {
        let sections: Vec<_> = (0..self.section.sections.len() + 2)
            .map(|i| {
                let has_blocks = i >= 1 && i - 1 < self.section.sections.len();
                let section = has_blocks.then(|| &self.section.sections[i - 1]);

                ChunkSectionNBT {
                    y: (i as i8) - 1i8 + section_coords::block_to_section(self.section.min_y) as i8,
                    block_states: section.map(|section| section.block_states.to_disk_nbt()),
                    biomes: section.map(|section| section.biomes.to_disk_nbt()),
                    block_light: match self.light_engine.block_light[i].clone() {
                        LightContainer::Empty(_) => None,
                        LightContainer::Full(data) => Some(data),
                    },
                    sky_light: match self.light_engine.sky_light[i].clone() {
                        LightContainer::Empty(_) => None,
                        LightContainer::Full(data) => Some(data),
                    },
                }
            })
            .filter(|nbt| {
                nbt.block_states.is_some()
                    || nbt.biomes.is_some()
                    || nbt.block_light.is_some()
                    || nbt.sky_light.is_some()
            })
            .collect();

        let nbt = ChunkNbt {
            data_version: WORLD_DATA_VERSION,
            x_pos: self.position.x,
            z_pos: self.position.z,
            min_y_section: section_coords::block_to_section(self.section.min_y),
            status: ChunkStatus::Full,
            heightmaps: self.heightmap.clone(),
            sections,
            block_ticks: {
                self.block_ticks
                    .iter()
                    .map(|tick| SerializedScheduledTick {
                        x: tick.block_pos.0.x,
                        y: tick.block_pos.0.y,
                        z: tick.block_pos.0.z,
                        delay: tick.delay as i32,
                        priority: tick.priority as i32,
                        target_block: format!(
                            "minecraft:{}",
                            Block::from_id(tick.target_block_id).unwrap().name
                        ),
                    })
                    .collect()
            },
            fluid_ticks: {
                self.fluid_ticks
                    .iter()
                    .map(|tick| SerializedScheduledTick {
                        x: tick.block_pos.0.x,
                        y: tick.block_pos.0.y,
                        z: tick.block_pos.0.z,
                        delay: tick.delay as i32,
                        priority: tick.priority as i32,
                        target_block: format!(
                            "minecraft:{}",
                            Block::from_id(tick.target_block_id).unwrap().name
                        ),
                    })
                    .collect()
            },
            block_entities: self
                .block_entities
                .values()
                .map(|block_entity| {
                    let mut nbt = NbtCompound::new();
                    block_entity.write_internal(&mut nbt);
                    nbt
                })
                .collect(),
            // we have not implemented light engine
            light_correct: false,
        };

        let mut result = Vec::new();
        pumpkin_nbt::to_bytes(&nbt, &mut result)
            .map_err(ChunkSerializingError::ErrorSerializingChunk)?;
        Ok(result.into())
    }
}

impl PathFromLevelFolder for ChunkEntityData {
    #[inline]
    fn file_path(folder: &LevelFolder, file_name: &str) -> PathBuf {
        folder.entities_folder.join(file_name)
    }
}

impl Dirtiable for ChunkEntityData {
    #[inline]
    fn mark_dirty(&mut self, flag: bool) {
        self.dirty = flag;
    }

    #[inline]
    fn is_dirty(&self) -> bool {
        self.dirty
    }
}

impl SingleChunkDataSerializer for ChunkEntityData {
    #[inline]
    fn from_bytes(bytes: Bytes, pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
        Self::internal_from_bytes(&bytes, pos).map_err(ChunkReadingError::ParsingError)
    }

    #[inline]
    fn to_bytes(&self) -> Result<Bytes, ChunkSerializingError> {
        self.internal_to_bytes()
    }

    #[inline]
    fn position(&self) -> &Vector2<i32> {
        &self.chunk_position
    }
}

impl ChunkEntityData {
    fn internal_from_bytes(
        chunk_data: &[u8],
        position: Vector2<i32>,
    ) -> Result<Self, ChunkParsingError> {
        let chunk_entity_data = from_bytes::<EntityNbt>(chunk_data)
            .map_err(|e| ChunkParsingError::ErrorDeserializingChunk(e.to_string()))?;

        if chunk_entity_data.position[0] != position.x
            || chunk_entity_data.position[1] != position.z
        {
            return Err(ChunkParsingError::ErrorDeserializingChunk(format!(
                "Expected data for entity chunk {},{} but got it for {},{}!",
                position.x,
                position.z,
                chunk_entity_data.position[0],
                chunk_entity_data.position[1],
            )));
        }

        // The 128-bit UUID is stored as four 32-bit integers ([Int] Ints), ordered from most to least significant.
        let entities: HashMap<Uuid, NbtCompound> = chunk_entity_data
            .entities
            .into_iter()
            .map(|data| {
                let uuid = data
                    .get_int_array("UUID")
                    .map_or_else(Uuid::new_v4, |array| {
                        Uuid::from_u128(
                            (array[0] as u128) << 96
                                | (array[1] as u128) << 64
                                | (array[2] as u128) << 32
                                | (array[3] as u128),
                        )
                    });
                (uuid, data)
            })
            .collect();

        Ok(ChunkEntityData {
            chunk_position: position,
            data: entities,
            // This chunk is read from disk, so it has not been modified
            dirty: false,
        })
    }

    fn internal_to_bytes(&self) -> Result<Bytes, ChunkSerializingError> {
        let nbt = EntityNbt {
            data_version: WORLD_DATA_VERSION,
            position: [self.chunk_position.x, self.chunk_position.z],
            entities: self.data.clone().into_values().collect(),
        };

        let mut result = Vec::new();
        pumpkin_nbt::to_bytes(&nbt, &mut result)
            .map_err(ChunkSerializingError::ErrorSerializingChunk)?;
        Ok(result.into())
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct ChunkSectionNBT {
    #[serde(skip_serializing_if = "Option::is_none")]
    block_states: Option<ChunkSectionBlockStates>,
    #[serde(skip_serializing_if = "Option::is_none")]
    biomes: Option<ChunkSectionBiomes>,
    #[serde(rename = "BlockLight", skip_serializing_if = "Option::is_none")]
    block_light: Option<Box<[u8]>>,
    #[serde(rename = "SkyLight", skip_serializing_if = "Option::is_none")]
    sky_light: Option<Box<[u8]>>,
    #[serde(rename = "Y")]
    y: i8,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChunkSectionBiomes {
    #[serde(
        serialize_with = "nbt_long_array",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) data: Option<Box<[i64]>>,
    pub(crate) palette: Vec<PaletteBiomeEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
// NOTE: Change not documented in the wiki; biome palettes are directly just the name now
#[serde(rename_all = "PascalCase", transparent)]
pub struct PaletteBiomeEntry {
    /// Biome name
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChunkSectionBlockStates {
    #[serde(
        serialize_with = "nbt_long_array",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) data: Option<Box<[i64]>>,
    pub(crate) palette: Vec<PaletteBlockEntry>,
}

#[derive(Debug, Clone)]
pub enum LightContainer {
    Empty(u8),
    Full(Box<[u8]>),
}

impl LightContainer {
    pub const DIM: usize = 16;
    pub const ARRAY_SIZE: usize = Self::DIM * Self::DIM * Self::DIM / 2;

    pub fn new_empty(default: u8) -> Self {
        if default > 15 {
            panic!("Default value must be between 0 and 15");
        }
        Self::Empty(default)
    }

    pub fn new(data: Box<[u8]>) -> Self {
        if data.len() != Self::ARRAY_SIZE {
            panic!("Data length must be {}", Self::ARRAY_SIZE);
        }
        Self::Full(data)
    }

    pub fn new_filled(default: u8) -> Self {
        if default > 15 {
            panic!("Default value must be between 0 and 15");
        }
        let value = default << 4 | default;
        Self::Full([value; Self::ARRAY_SIZE].into())
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty(_))
    }

    fn index(x: usize, y: usize, z: usize) -> usize {
        y * 16 * 16 + z * 16 + x
    }

    pub fn get(&self, x: usize, y: usize, z: usize) -> u8 {
        match self {
            Self::Full(data) => {
                let index = Self::index(x, y, z);
                data[index >> 1] >> (4 * (index & 1)) & 0x0F
            }
            Self::Empty(default) => *default,
        }
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, value: u8) {
        match self {
            Self::Full(data) => {
                let index = Self::index(x, y, z);
                let mask = 0x0F << (4 * (index & 1));
                data[index >> 1] &= !mask;
                data[index >> 1] |= value << (4 * (index & 1));
            }
            Self::Empty(default) => {
                if value != *default {
                    *self = Self::new_filled(*default);
                    self.set(x, y, z, value);
                }
            }
        }
    }
}

impl Default for LightContainer {
    fn default() -> Self {
        Self::new_empty(15)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PaletteBlockEntry {
    /// Block name
    pub name: String,
    /// Key-value pairs of properties
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug)]
struct SerializedScheduledTick {
    #[serde(rename = "x")]
    x: i32,
    #[serde(rename = "y")]
    y: i32,
    #[serde(rename = "z")]
    z: i32,
    #[serde(rename = "t")]
    delay: i32,
    #[serde(rename = "p")]
    priority: i32,
    #[serde(rename = "i")]
    target_block: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct ChunkNbt {
    data_version: i32,
    #[serde(rename = "xPos")]
    x_pos: i32,
    #[serde(rename = "zPos")]
    z_pos: i32,
    #[serde(rename = "yPos")]
    min_y_section: i32,
    status: ChunkStatus,
    #[serde(rename = "sections")]
    sections: Vec<ChunkSectionNBT>,
    heightmaps: ChunkHeightmaps,
    #[serde(rename = "block_ticks")]
    block_ticks: Vec<SerializedScheduledTick>,
    #[serde(rename = "fluid_ticks")]
    fluid_ticks: Vec<SerializedScheduledTick>,
    #[serde(rename = "block_entities")]
    block_entities: Vec<NbtCompound>,
    #[serde(rename = "isLightOn")]
    light_correct: bool,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct EntityNbt {
    data_version: i32,
    position: [i32; 2],
    entities: Vec<NbtCompound>,
}
