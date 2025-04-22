use std::collections::HashMap;

use pumpkin_data::{block::Block, chunk::ChunkStatus};
use pumpkin_nbt::{compound::NbtCompound, from_bytes, nbt_long_array};

use pumpkin_util::math::{position::BlockPos, vector2::Vector2};
use serde::{Deserialize, Serialize};

use crate::{block::entities::block_entity_from_nbt, generation::section_coords};

use super::{
    ChunkData, ChunkHeightmaps, ChunkParsingError, ChunkSections, ScheduledTick, SubChunk,
    TickPriority,
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

impl ChunkData {
    pub fn from_bytes(
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
            log::debug!(
                "reading light_correct chunk data for chunk {},{}, min_y={}, first_y={}",
                position.x,
                position.z,
                chunk_data.min_y_section,
                chunk_data.sections.first().map(|s| s.y).unwrap_or(-99)
            );
            for section in chunk_data.sections.clone() {
                let mut block = false;
                let mut sky = false;
                let mut block_sum = 0;
                let mut sky_sum = 0;
                if let Some(block_light) = &section.block_light {
                    block = block_light.len() > 0;
                    block_sum = block_light
                        .iter()
                        .map(|b| ((*b >> 4) + (*b & 0x0F)) as usize)
                        .sum();
                }
                if let Some(sky_light) = &section.sky_light {
                    sky = sky_light.len() > 0;
                    sky_sum = sky_light
                        .iter()
                        .map(|b| ((*b >> 4) + (*b & 0x0F)) as usize)
                        .sum();
                }
                if block || sky {
                    log::debug!(
                        "section {}: block_light={}/{}, sky_light={}/{}",
                        section.y,
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
                block_light: section.block_light,
                sky_light: section.sky_light,
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
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
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
