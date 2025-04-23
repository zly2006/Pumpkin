use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};

use crate::{ClientPacket, ServerPacket, codec::var_int::VarIntType};

use super::{
    Read, ReadingError, Write, WritingError, deserializer,
    serializer::{self},
};

pub trait Packet {
    const PACKET_ID: VarIntType;
}

#[async_trait]
impl<P> ClientPacket for P
where
    P: Packet + Serialize + Sync,
{
    async fn write_packet_data(&self, write: impl Write + Send) -> Result<(), WritingError> {
        let mut serializer = serializer::Serializer::new(write);
        self.serialize(&mut serializer)
    }
}

impl<P> ServerPacket for P
where
    P: Packet + DeserializeOwned,
{
    fn read(read: impl Read) -> Result<P, ReadingError> {
        let mut deserializer = deserializer::Deserializer::new(read);
        P::deserialize(&mut deserializer)
    }
}
