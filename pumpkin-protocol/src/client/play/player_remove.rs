use pumpkin_data::packet::clientbound::PLAY_PLAYER_INFO_REMOVE;
use pumpkin_macros::packet;
use serde::{Serialize, ser::SerializeSeq};

#[derive(Serialize)]
#[packet(PLAY_PLAYER_INFO_REMOVE)]
pub struct CRemovePlayerInfo<'a> {
    #[serde(serialize_with = "serialize_slice_uuids")]
    players: &'a [uuid::Uuid],
}

impl<'a> CRemovePlayerInfo<'a> {
    pub fn new(players: &'a [uuid::Uuid]) -> Self {
        Self { players }
    }
}

fn serialize_slice_uuids<S: serde::Serializer>(
    uuids: &[uuid::Uuid],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut seq = serializer.serialize_seq(Some(uuids.len()))?;
    for uuid in uuids {
        seq.serialize_element(uuid.as_bytes())?;
    }
    seq.end()
}
