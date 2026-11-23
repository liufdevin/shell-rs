//! ST AN3155 USART bootloader wire protocol (also used by compatible GD32s).
//! All capabilities and bounds are checked before the first erase command.
use std::time::Duration;

use super::{Firmware, IspRequest};
use crate::i18n::t;

pub(crate) trait Link {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn read(&mut self, count: usize, timeout: Duration) -> Result<Vec<u8>, String>;
}

const ACK: u8 = 0x79;
const FLASH: u32 = 0x0800_0000;

fn ack(link: &mut impl Link, timeout: Duration) -> Result<(), String> {
    let byte = link.read(1, timeout)?[0];
    if byte == ACK {
        Ok(())
    } else {
        Err(t!("serial.isp.nack", byte = format!("{byte:02X}")).to_string())
    }
}

fn command(link: &mut impl Link, byte: u8) -> Result<(), String> {
    link.write(&[byte, !byte])?;
    ack(link, Duration::from_secs(2))
}

fn address(link: &mut impl Link, at: u32) -> Result<(), String> {
    let mut packet = at.to_be_bytes().to_vec();
    packet.push(packet.iter().fold(0, |checksum, byte| checksum ^ byte));
    link.write(&packet)?;
    ack(link, Duration::from_secs(2))
}

fn capacity(id: u16) -> Option<u32> {
    let kib = match id {
        0x410 | 0x420 | 0x448 => 128,
        0x412 | 0x444 | 0x445 => 32,
        0x414 | 0x428 | 0x431 | 0x433 => 512,
        0x418 | 0x442 => 256,
        0x430 | 0x413 => 1024,
        0x419 => 2048,
        0x440 => 64,
        _ => return None,
    };
    Some(kib * 1024)
}

fn bounds(image: &Firmware, size: u32) -> Result<(), String> {
    let padded = image.bytes.len().div_ceil(4) * 4;
    if !image.address.is_multiple_of(4)
        || image.address < FLASH
        || image
            .address
            .checked_add(padded as u32)
            .is_none_or(|end| end > FLASH + size)
    {
        return Err(t!("serial.isp.address_range", size = size / 1024).to_string());
    }
    Ok(())
}

fn read_memory(link: &mut impl Link, at: u32, count: usize) -> Result<Vec<u8>, String> {
    command(link, 0x11)?;
    address(link, at)?;
    let count = (count - 1) as u8;
    link.write(&[count, !count])?;
    ack(link, Duration::from_secs(2))?;
    link.read(usize::from(count) + 1, Duration::from_secs(3))
}

#[cfg(test)]
fn execute(link: &mut impl Link, request: &IspRequest) -> Result<(), String> {
    execute_with_sync(link, request, true)
}

pub(crate) fn execute_with_sync(
    link: &mut impl Link,
    request: &IspRequest,
    synchronize: bool,
) -> Result<(), String> {
    request.check_cancelled()?;
    request.report(0, t!("serial.isp.synchronizing").to_string());
    if synchronize {
        link.write(&[0x7f])?;
        ack(link, Duration::from_secs(3))?;
    }
    command(link, 0x00)?;
    let count = usize::from(link.read(1, Duration::from_secs(2))?[0]) + 1;
    let supported = link.read(count, Duration::from_secs(2))?;
    ack(link, Duration::from_secs(2))?;
    if supported.len() < 2 || !supported[1..].contains(&0x02) {
        return Err(t!("serial.isp.unsupported").to_string());
    }
    command(link, 0x02)?;
    let count = usize::from(link.read(1, Duration::from_secs(2))?[0]) + 1;
    if count != 2 {
        return Err(t!("serial.isp.unsupported").to_string());
    }
    let id = link.read(2, Duration::from_secs(2))?;
    let id = u16::from_be_bytes([id[0], id[1]]);
    ack(link, Duration::from_secs(2))?;
    request.identify(
        t!(
            "serial.isp.chip",
            id = format!("{id:04X}"),
            version = format!("{:02X}", supported[0])
        )
        .to_string(),
    );
    let Some(image) = &request.image else {
        return Ok(());
    };
    let supported = &supported[1..];
    let erase = if supported.contains(&0x44) {
        0x44
    } else {
        0x43
    };
    if !supported.contains(&erase)
        || !supported.contains(&0x31)
        || !supported.contains(&0x11)
        || request.run && !supported.contains(&0x21)
    {
        return Err(t!("serial.isp.unsupported").to_string());
    }
    let maximum = capacity(id)
        .ok_or_else(|| t!("serial.isp.unknown_capacity", id = format!("{id:04X}")).to_string())?;
    let size_at = match id {
        0x410 | 0x412 | 0x414 | 0x418 | 0x420 | 0x428 | 0x430 => 0x1fff_f7e0,
        0x413 | 0x419 | 0x431 | 0x433 => 0x1fff_7a22,
        _ => 0x1fff_f7cc,
    };
    let size = read_memory(link, size_at, 2)?;
    let size = u32::from(u16::from_le_bytes([size[0], size[1]])) * 1024;
    if size == 0 || size > maximum {
        return Err(t!("serial.isp.unknown_capacity", id = format!("{id:04X}")).to_string());
    }
    bounds(image, size)?;
    request.check_cancelled()?;
    request.report(10, t!("serial.isp.erasing").to_string());
    command(link, erase)?;
    link.write(if erase == 0x44 {
        &[0xff, 0xff, 0x00]
    } else {
        &[0xff, 0x00]
    })?;
    ack(link, Duration::from_secs(60))?;
    for (index, block) in image.bytes.chunks(256).enumerate() {
        request.check_cancelled()?;
        let offset = index * 256;
        command(link, 0x31)?;
        address(link, image.address + offset as u32)?;
        let length = block.len().div_ceil(4) * 4;
        let mut packet = vec![(length - 1) as u8];
        packet.extend_from_slice(block);
        packet.resize(length + 1, 0xff);
        packet.push(packet.iter().fold(0, |checksum, byte| checksum ^ byte));
        link.write(&packet)?;
        ack(link, Duration::from_secs(5))?;
        request.report(
            (10 + (offset + block.len()) * 60 / image.bytes.len()) as u8,
            t!(
                "serial.isp.writing",
                done = offset + block.len(),
                total = image.bytes.len()
            )
            .to_string(),
        );
    }
    if request.verify {
        for (index, block) in image.bytes.chunks(256).enumerate() {
            request.check_cancelled()?;
            let offset = index * 256;
            if read_memory(link, image.address + offset as u32, block.len())? != block {
                return Err(t!(
                    "serial.isp.verify_failed",
                    address = format!("{:08X}", image.address + offset as u32)
                )
                .to_string());
            }
            request.report(
                (70 + (offset + block.len()) * 25 / image.bytes.len()) as u8,
                t!("serial.isp.verifying").to_string(),
            );
        }
    }
    if request.run {
        request.check_cancelled()?;
        request.report(98, t!("serial.isp.starting").to_string());
        command(link, 0x21)?;
        address(link, image.address)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, atomic::AtomicBool, mpsc},
    };

    struct Wire {
        received: VecDeque<u8>,
        sent: Vec<Vec<u8>>,
        cancel_on_data: Option<Arc<AtomicBool>>,
    }
    impl Link for Wire {
        fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.sent.push(bytes.to_vec());
            if bytes.len() > 256
                && let Some(cancel) = &self.cancel_on_data
            {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            Ok(())
        }
        fn read(&mut self, count: usize, _: Duration) -> Result<Vec<u8>, String> {
            if self.received.len() < count {
                return Err("timeout".into());
            }
            Ok((0..count)
                .map(|_| self.received.pop_front().unwrap())
                .collect())
        }
    }
    fn setup(extended: bool) -> (Wire, IspRequest) {
        let mut hello = vec![
            ACK,
            ACK,
            6,
            0x31,
            0x00,
            0x02,
            0x11,
            0x21,
            0x31,
            if extended { 0x44 } else { 0x43 },
            ACK,
            ACK,
            1,
            4,
            0x10,
            ACK,
        ];
        hello.extend([ACK; 3]); // read actual flash size, not the family's maximum
        hello.extend([128, 0]);
        hello.extend([ACK; 5]); // erase command/data; write command/address/data
        hello.extend([ACK; 3]); // read command/address/length
        hello.extend([1, 2, 3]);
        hello.extend([ACK; 2]); // go command/address
        (
            Wire {
                received: hello.into(),
                sent: vec![],
                cancel_on_data: None,
            },
            IspRequest {
                image: Some(Firmware {
                    address: FLASH,
                    bytes: vec![1, 2, 3],
                }),
                baud: 115200,
                verify: true,
                run: true,
                stc: None,
                cancelled: Arc::new(AtomicBool::new(false)),
                active: Arc::new(AtomicBool::new(true)),
                events: mpsc::sync_channel(128).0,
            },
        )
    }
    #[test]
    fn erase_write_padding_checksum_verify_and_go_follow_the_wire_protocol() {
        for extended in [false, true] {
            let (mut wire, request) = setup(extended);
            execute(&mut wire, &request).unwrap();
            assert_eq!(wire.sent[0], [0x7f]);
            assert!(wire.sent.contains(&if extended {
                vec![255, 255, 0]
            } else {
                vec![255, 0]
            }));
            assert!(wire.sent.contains(&vec![3, 1, 2, 3, 255, 252]));
            assert_eq!(wire.sent.last().unwrap(), &[8, 0, 0, 0, 8]);
            assert!(wire.received.is_empty());
        }
    }
    #[test]
    fn invalid_address_unknown_chip_nack_timeout_and_cancellation_never_erase() {
        for at in [0, FLASH + 128 * 1024, FLASH + 1] {
            let (mut wire, mut request) = setup(true);
            request.image.as_mut().unwrap().address = at;
            assert!(execute(&mut wire, &request).is_err());
            assert!(!wire.sent.contains(&vec![0x44, 0xbb]));
        }
        let (mut wire, request) = setup(true);
        wire.received[13] = 0xff;
        assert!(execute(&mut wire, &request).is_err());
        assert!(!wire.sent.contains(&vec![0x44, 0xbb]));
        let (mut wire, request) = setup(true);
        wire.received[0] = 0x1f;
        assert!(execute(&mut wire, &request).is_err());
        wire.received.clear();
        assert!(execute(&mut wire, &request).is_err());
        request
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        wire.sent.clear();
        assert!(execute(&mut wire, &request).is_err());
        assert!(wire.sent.is_empty());
    }
    #[test]
    fn actual_capacity_and_required_commands_are_checked_before_erasing() {
        let (mut wire, mut request) = setup(true);
        wire.received[19] = 64; // A 64 KiB part with the same 0x410 family PID.
        request.image.as_mut().unwrap().address = FLASH + 64 * 1024;
        assert!(execute(&mut wire, &request).is_err());
        assert!(!wire.sent.contains(&vec![0x44, 0xbb]));
        let (mut wire, request) = setup(true);
        wire.received[6] = 0x99; // Read Memory was not advertised.
        assert!(execute(&mut wire, &request).is_err());
        assert!(!wire.sent.contains(&vec![0x44, 0xbb]));
        let (mut wire, request) = setup(true);
        wire.received[19] = 0;
        assert!(execute(&mut wire, &request).is_err());
        assert!(!wire.sent.contains(&vec![0x44, 0xbb]));
    }

    #[test]
    fn an_existing_bootloader_connection_can_be_used_without_another_sync_byte() {
        let (mut wire, request) = setup(false);
        wire.received.pop_front(); // Bootloader already synchronized by Detect.
        execute_with_sync(&mut wire, &request, false).unwrap();
        assert_eq!(wire.sent[0], [0x00, 0xff]);
        assert!(!wire.sent.contains(&vec![0x7f]));
    }
    #[test]
    fn multiple_blocks_advance_addresses_and_can_skip_verification_and_go() {
        let (mut wire, mut request) = setup(true);
        request.image.as_mut().unwrap().bytes = (0..300).map(|value| value as u8).collect();
        request.verify = false;
        request.run = false;
        wire.received.truncate(26);
        wire.received.extend([ACK; 3]);
        execute(&mut wire, &request).unwrap();
        assert!(wire.sent.contains(&vec![8, 0, 0, 0, 8]));
        assert!(wire.sent.contains(&vec![8, 0, 1, 0, 9]));
        let packets: Vec<_> = wire.sent.iter().filter(|bytes| bytes.len() > 5).collect();
        assert_eq!(packets[0].len(), 258);
        assert_eq!(packets[0][0], 255);
        assert_eq!(
            &packets[1][1..45],
            &request.image.as_ref().unwrap().bytes[256..]
        );
        assert!(!wire.sent.contains(&vec![0x21, 0xde]));
        assert!(wire.received.is_empty());
    }

    #[test]
    fn cancellation_after_a_write_prevents_the_next_block() {
        let (mut wire, mut request) = setup(true);
        request.image.as_mut().unwrap().bytes = vec![0xaa; 300];
        wire.cancel_on_data = Some(request.cancelled.clone());
        assert!(execute(&mut wire, &request).is_err());
        assert_eq!(
            wire.sent
                .iter()
                .filter(|bytes| **bytes == [0x31, 0xce])
                .count(),
            1
        );
        assert!(!wire.sent.contains(&vec![8, 0, 1, 0, 9]));
    }
    #[test]
    fn bytes_from_the_started_application_are_not_consumed_as_protocol_replies() {
        let (mut wire, request) = setup(true);
        wire.received.extend(b"boot\r\n");
        execute(&mut wire, &request).unwrap();
        assert_eq!(wire.received.into_iter().collect::<Vec<_>>(), b"boot\r\n");
    }
    #[test]
    fn verification_mismatch_stops_before_go_and_detection_never_writes_flash() {
        let (mut wire, request) = setup(true);
        wire.received[29] ^= 1;
        assert!(execute(&mut wire, &request).is_err());
        assert!(!wire.sent.contains(&vec![0x21, 0xde]));
        let (mut wire, mut request) = setup(true);
        request.image = None;
        execute(&mut wire, &request).unwrap();
        assert_eq!(wire.sent.len(), 3);
    }
}
