use std::io;
use std::io::prelude::*;
use std::io::Cursor;
use std::net;
use std::net::SocketAddrV6;

extern crate byteorder;
use byteorder::{BigEndian, ByteOrder, LittleEndian, ReadBytesExt};

extern crate ed25519_dalek;
use ed25519_dalek::PublicKey;

extern crate tokio_codec;

extern crate bytes;
use bytes::{BufMut, BytesMut};

extern crate nanocurrency_types;
use nanocurrency_types::*;

#[cfg(test)]
mod tests;

const NET_VERSION: u8 = 0x10;
const NET_VERSION_MAX: u8 = 0x10;
const NET_VERSION_MIN: u8 = 0x01;

const NODE_ID_HANDSHAKE_QUERY_FLAG: u16 = 1 << 0;
const NODE_ID_HANDSHAKE_RESPONSE_FLAG: u16 = 1 << 1;

trait BufMutExt: BufMut {
    fn put_i128_le(&mut self, n: i128) {
        let mut buf = [0u8; 16];
        LittleEndian::write_i128(&mut buf, n);
        self.put_slice(&buf)
    }

    fn put_i128_be(&mut self, n: i128) {
        let mut buf = [0u8; 16];
        BigEndian::write_i128(&mut buf, n);
        self.put_slice(&buf)
    }

    fn put_u128_le(&mut self, n: u128) {
        let mut buf = [0u8; 16];
        LittleEndian::write_u128(&mut buf, n);
        self.put_slice(&buf)
    }

    fn put_u128_be(&mut self, n: u128) {
        let mut buf = [0u8; 16];
        BigEndian::write_u128(&mut buf, n);
        self.put_slice(&buf)
    }
}

impl BufMutExt for BytesMut {}

// Note: this does not include the message type.
// That's wrapped into the Message enum.
#[allow(dead_code)]
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct MessageHeader {
    pub network: Network,
    pub version_max: u8,
    pub version: u8,
    pub version_min: u8,
    pub extensions: u16,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Message {
    Keepalive([SocketAddrV6; 8]),
    Publish(Block),
    ConfirmReq(Block),
    ConfirmAck(Vote),
    NodeIdHandshake(Option<[u8; 32]>, Option<(PublicKey, Signature)>),
}

pub struct NanoCurrencyCodec;

impl NanoCurrencyCodec {
    pub fn read_block<C: io::Read>(cursor: &mut C, block_ty: u8) -> io::Result<Block> {
        let inner = match block_ty {
            2 => {
                // send
                let mut previous = BlockHash::default();
                cursor.read_exact(&mut previous.0)?;
                let mut destination = Account::default();
                cursor.read_exact(&mut destination.0)?;
                let balance = cursor.read_u128::<BigEndian>()?;
                BlockInner::Send {
                    previous,
                    destination,
                    balance,
                }
            }
            3 => {
                // receieve
                let mut previous = BlockHash::default();
                cursor.read_exact(&mut previous.0)?;
                let mut source = BlockHash::default();
                cursor.read_exact(&mut source.0)?;
                BlockInner::Receive { previous, source }
            }
            4 => {
                // open
                let mut source = BlockHash::default();
                cursor.read_exact(&mut source.0)?;
                let mut representative = Account::default();
                cursor.read_exact(&mut representative.0)?;
                let mut account = Account::default();
                cursor.read_exact(&mut account.0)?;
                BlockInner::Open {
                    source,
                    representative,
                    account,
                }
            }
            5 => {
                // change
                let mut previous = BlockHash::default();
                cursor.read_exact(&mut previous.0)?;
                let mut representative = Account::default();
                cursor.read_exact(&mut representative.0)?;
                BlockInner::Change {
                    previous,
                    representative,
                }
            }
            6 => {
                // state
                let mut account = Account::default();
                cursor.read_exact(&mut account.0)?;
                let mut previous = BlockHash::default();
                cursor.read_exact(&mut previous.0)?;
                let mut representative = Account::default();
                cursor.read_exact(&mut representative.0)?;
                let balance = cursor.read_u128::<BigEndian>()?;
                let mut link = [0u8; 32];
                cursor.read_exact(&mut link)?;
                BlockInner::State {
                    account,
                    previous,
                    representative,
                    balance,
                    link,
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "unrecognized block type",
                ))
            }
        };
        let mut signature = [0u8; 64];
        cursor.read_exact(&mut signature)?;
        let signature = Signature::from_bytes(&signature)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "bad signature"))?;
        let work;
        if block_ty >= 6 {
            // New block types have work in big endian
            work = cursor.read_u64::<BigEndian>()?;
        } else {
            work = cursor.read_u64::<LittleEndian>()?;
        }
        let header = BlockHeader { signature, work };
        Ok(Block { header, inner })
    }

    pub fn block_type_num(block: &Block) -> u8 {
        match block.inner {
            BlockInner::Send { .. } => 2,
            BlockInner::Receive { .. } => 3,
            BlockInner::Open { .. } => 4,
            BlockInner::Change { .. } => 5,
            BlockInner::State { .. } => 6,
        }
    }

    /// Does NOT include block type
    pub fn write_block(buf: &mut BytesMut, block: Block) {
        buf.reserve(block.size());
        let mut work_big_endian = false;
        match block.inner {
            BlockInner::Send {
                previous,
                destination,
                balance,
            } => {
                buf.put_slice(&previous.0);
                buf.put_slice(&destination.0);
                buf.put_u128_be(balance);
            }
            BlockInner::Receive { previous, source } => {
                buf.put_slice(&previous.0);
                buf.put_slice(&source.0);
            }
            BlockInner::Open {
                source,
                representative,
                account,
            } => {
                buf.put_slice(&source.0);
                buf.put_slice(&representative.0);
                buf.put_slice(&account.0);
            }
            BlockInner::Change {
                previous,
                representative,
            } => {
                buf.put_slice(&previous.0);
                buf.put_slice(&representative.0);
            }
            BlockInner::State {
                account,
                previous,
                representative,
                balance,
                link,
            } => {
                buf.put_slice(&account.0);
                buf.put_slice(&previous.0);
                buf.put_slice(&representative.0);
                buf.put_u128_be(balance);
                buf.put_slice(&link as &[u8]);
                work_big_endian = true;
            }
        };
        buf.put_slice(&block.header.signature.to_bytes() as &[u8]);
        if work_big_endian {
            buf.put_u64_be(block.header.work);
        } else {
            buf.put_u64_le(block.header.work);
        }
    }

    pub fn network_magic_byte(network: Network) -> u8 {
        match network {
            Network::Test => b'A',
            Network::Beta => b'B',
            Network::Live => b'C',
        }
    }
}

// Message types:
// invalid      0
// not_a_type   1
// keepalive    2
// publish      3
// confirm_req  4
// confirm_ack  5
//
// Bootstrap message types:
// bulk_pull    6
// bulk_push    7
// frontier_req 8

impl tokio_codec::Decoder for NanoCurrencyCodec {
    type Item = (MessageHeader, Message);
    type Error = io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        let mut cursor = Cursor::new(buf);
        if cursor.read_u8()? != b'R' {
            return Err(io::Error::new(io::ErrorKind::Other, "invalid magic number"));
        }
        let network = match cursor.read_u8()? {
            b'A' => Network::Test,
            b'B' => Network::Beta,
            b'C' => Network::Live,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "invalid network indicator",
                ))
            }
        };
        let version_max = cursor.read_u8()?;
        let version = cursor.read_u8()?;
        let version_min = cursor.read_u8()?;
        let msg_type = cursor.read_u8()?;
        let extensions = cursor.read_u16::<LittleEndian>()?;
        if version_min > NET_VERSION_MAX || version_max < NET_VERSION_MIN {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "unsupported peer version",
            ));
        }
        let header = MessageHeader {
            network,
            version_max,
            version,
            version_min,
            extensions,
        };
        let message = match msg_type {
            2 => {
                // keepalive
                let mut peers = [zero_v6_addr!(); 8];
                let _ = (|| -> io::Result<()> {
                    for peer in peers.iter_mut() {
                        let mut ip_bytes: [u8; 16] = [0; 16];
                        for byte in ip_bytes.iter_mut() {
                            *byte = cursor.read_u8()?;
                        }
                        let port = cursor.read_u16::<LittleEndian>()?;
                        *peer = SocketAddrV6::new(net::Ipv6Addr::from(ip_bytes), port, 0, 0);
                    }
                    Ok(())
                })();
                Message::Keepalive(peers)
            }
            3 => {
                // publish
                let ty = (header.extensions & 0x0f00) >> 8;
                Message::Publish(Self::read_block(&mut cursor, ty as u8)?)
            }
            4 => {
                // confirm_req
                let ty = (header.extensions & 0x0f00) >> 8;
                Message::ConfirmReq(Self::read_block(&mut cursor, ty as u8)?)
            }
            5 => {
                // confirm_ack
                let ty = (header.extensions & 0x0f00) >> 8;
                let mut account = Account::default();
                cursor.read_exact(&mut account.0)?;
                let mut signature = [0u8; 64];
                cursor.read_exact(&mut signature)?;
                let signature = Signature::from_bytes(&signature).unwrap();
                let sequence = cursor.read_u64::<LittleEndian>()?;
                let block = Self::read_block(&mut cursor, ty as u8)?;
                Message::ConfirmAck(Vote {
                    account,
                    signature,
                    sequence,
                    block,
                })
            }
            10 => {
                // node_id_handshake
                let query = if header.extensions & NODE_ID_HANDSHAKE_QUERY_FLAG != 0 {
                    let mut query = [0u8; 32];
                    cursor.read_exact(&mut query)?;
                    Some(query)
                } else {
                    None
                };
                let response = if header.extensions & NODE_ID_HANDSHAKE_RESPONSE_FLAG != 0 {
                    let mut pubkey = [0u8; 32];
                    cursor.read_exact(&mut pubkey)?;
                    let pubkey = PublicKey::from_bytes(&pubkey)
                        .map_err(|_| io::Error::new(io::ErrorKind::Other, "bad pubkey"))?;
                    let mut signature = [0u8; 64];
                    cursor.read_exact(&mut signature)?;
                    let signature = Signature::from_bytes(&signature)
                        .map_err(|_| io::Error::new(io::ErrorKind::Other, "bad signature"))?;
                    Some((pubkey, signature))
                } else {
                    None
                };
                Message::NodeIdHandshake(query, response)
            }
            6 | 7 | 8 => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "bootstrap message sent over UDP",
                ))
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "unrecognized message type",
                ))
            }
        };
        Ok(Some((header, message)))
    }
}

impl tokio_codec::Encoder for NanoCurrencyCodec {
    type Item = (Network, Message);
    type Error = io::Error;

    fn encode(&mut self, msg: Self::Item, buf: &mut BytesMut) -> io::Result<()> {
        buf.reserve(8); // header (including extensions)
        buf.put_slice(&[
            b'R',
            Self::network_magic_byte(msg.0),
            NET_VERSION_MAX,
            NET_VERSION,
            NET_VERSION_MIN,
        ]);
        match msg.1 {
            Message::Keepalive(peers) => {
                buf.put_slice(&[2]);
                buf.put_slice(&[0, 0]); // extensions
                buf.reserve(peers.len() * (16 + 2));
                for peer in peers.iter() {
                    buf.put_slice(&peer.ip().octets());
                    buf.put_u16_le(peer.port());
                }
            }
            Message::Publish(block) => {
                buf.put_slice(&[3]);
                let type_num = Self::block_type_num(&block) as u16;
                buf.put_u16_le((type_num & 0x0f) << 8);
                Self::write_block(buf, block);
            }
            Message::ConfirmReq(block) => {
                buf.put_slice(&[4]);
                let type_num = Self::block_type_num(&block) as u16;
                buf.put_u16_le((type_num & 0x0f) << 8);
                Self::write_block(buf, block);
            }
            Message::ConfirmAck(Vote {
                account,
                signature,
                sequence,
                block,
            }) => {
                buf.put_slice(&[5]);
                let type_num = Self::block_type_num(&block) as u16;
                buf.put_u16_le((type_num & 0x0f) << 8);
                buf.reserve(32 + 64 + 8);
                buf.put_slice(&account.0);
                buf.put_slice(&signature.to_bytes());
                buf.put_u64_le(sequence);
                Self::write_block(buf, block);
            }
            Message::NodeIdHandshake(query, response) => {
                buf.put_slice(&[10]);
                let mut flags = 0;
                let mut len = 0;
                if query.is_some() {
                    flags |= NODE_ID_HANDSHAKE_QUERY_FLAG;
                    len += 32;
                }
                if response.is_some() {
                    flags |= NODE_ID_HANDSHAKE_RESPONSE_FLAG;
                    len += 32 + 64;
                }
                buf.put_u16_le(flags);
                buf.reserve(len);
                if let Some(query) = query {
                    buf.put_slice(&query);
                }
                if let Some(response) = response {
                    buf.put_slice(&response.0.to_bytes());
                    buf.put_slice(&response.1.to_bytes());
                }
            }
        }
        Ok(())
    }
}
