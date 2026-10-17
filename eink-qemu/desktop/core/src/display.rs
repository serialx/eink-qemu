//! Reader for QEMU's `-display eink` stream (ui/eink.c). Records are
//! little-endian: u32 type, u32 payload length, payload.
//!   1 HELLO  "EINK", u32 version (1), u32 width, u32 height
//!   2 RESIZE u32 width, u32 height
//!   3 RECT   u32 x, y, w, h, then w*h gray8 pixels
//!   4 FLUSH  end of a refresh that sent rectangles
//!
//! Each FLUSH becomes one message for the WebView, little-endian:
//!   size:  u32 1, u32 width, u32 height
//!   frame: u32 2, u32 count, then per rect u32 x, y, w, h and w*h pixels

use tokio::io::{AsyncRead, AsyncReadExt};

const HELLO: u32 = 1;
const RESIZE: u32 = 2;
const RECT: u32 = 3;
const FLUSH: u32 = 4;
const MAX_SIDE: u32 = 4096;
const MAX_PIXELS: usize = 4096 * 4096;
const MAX_RECTS: usize = 1024;

pub const MSG_SIZE: u32 = 1;
pub const MSG_FRAME: u32 = 2;

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_string())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

pub fn size_message(width: u32, height: u32) -> Vec<u8> {
    [MSG_SIZE, width, height].iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Read records until the stream ends. `on_message` receives each WebView
/// message; the first size message means the display is connected.
pub async fn read<R, F>(mut stream: R, mut on_message: F) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(Vec<u8>),
{
    let (mut width, mut height) = (0u32, 0u32);
    let mut greeted = false;
    let mut frame: Vec<u8> = Vec::new();
    let mut rects = 0usize;
    let mut pixels = 0usize;
    let mut header = [0u8; 8];
    loop {
        match stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }
        let kind = u32_at(&header, 0);
        let length = u32_at(&header, 4) as usize;
        let valid = match kind {
            HELLO => length == 16,
            RESIZE => length == 8,
            RECT => (16..=16 + MAX_PIXELS).contains(&length),
            FLUSH => length == 0,
            _ => false,
        };
        if !valid || (!greeted && kind != HELLO) {
            return Err(invalid("Invalid eink display record."));
        }
        let mut payload = vec![0u8; length];
        stream.read_exact(&mut payload).await?;
        let mut resize = |w: u32, h: u32| -> std::io::Result<Vec<u8>> {
            if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || (w * h) as usize > MAX_PIXELS {
                return Err(invalid("Invalid eink panel dimensions."));
            }
            width = w;
            height = h;
            Ok(size_message(w, h))
        };
        match kind {
            HELLO => {
                if greeted || &payload[0..4] != b"EINK" || u32_at(&payload, 4) != 1 {
                    return Err(invalid("Unsupported eink display protocol: rebuild QEMU."));
                }
                greeted = true;
                on_message(resize(u32_at(&payload, 8), u32_at(&payload, 12))?);
                frame.clear();
                rects = 0;
                pixels = 0;
            }
            RESIZE => {
                on_message(resize(u32_at(&payload, 0), u32_at(&payload, 4))?);
                frame.clear();
                rects = 0;
                pixels = 0;
            }
            RECT => {
                let (x, y, w, h) = (u32_at(&payload, 0), u32_at(&payload, 4), u32_at(&payload, 8), u32_at(&payload, 12));
                let area = w as usize * h as usize;
                if w == 0 || h == 0 || x.checked_add(w).is_none_or(|r| r > width)
                    || y.checked_add(h).is_none_or(|b| b > height) || payload.len() != 16 + area
                    || rects >= MAX_RECTS || pixels + area > MAX_PIXELS
                {
                    return Err(invalid("Invalid eink rectangle."));
                }
                if frame.is_empty() {
                    frame.extend_from_slice(&MSG_FRAME.to_le_bytes());
                    frame.extend_from_slice(&0u32.to_le_bytes());
                }
                frame.extend_from_slice(&payload);
                rects += 1;
                pixels += area;
            }
            FLUSH => {
                if rects > 0 {
                    frame[4..8].copy_from_slice(&(rects as u32).to_le_bytes());
                    on_message(std::mem::take(&mut frame));
                }
                frame.clear();
                rects = 0;
                pixels = 0;
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = kind.to_le_bytes().to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }
    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[tokio::test]
    async fn hello_rect_flush() {
        let mut hello = b"EINK".to_vec();
        hello.extend(words(&[1, 4, 2]));
        let mut rect = words(&[1, 0, 2, 2]);
        rect.extend([10, 20, 30, 40]);
        let mut stream = record(HELLO, &hello);
        stream.extend(record(RECT, &rect));
        stream.extend(record(FLUSH, &[]));
        stream.extend(record(FLUSH, &[]));
        let mut messages = Vec::new();
        read(&stream[..], |m| messages.push(m)).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], size_message(4, 2));
        let mut expected = words(&[MSG_FRAME, 1, 1, 0, 2, 2]);
        expected.extend([10, 20, 30, 40]);
        assert_eq!(messages[1], expected);
    }

    #[tokio::test]
    async fn rejects_out_of_bounds() {
        let mut hello = b"EINK".to_vec();
        hello.extend(words(&[1, 4, 2]));
        let mut rect = words(&[3, 0, 2, 1]);
        rect.extend([0, 0]);
        let mut stream = record(HELLO, &hello);
        stream.extend(record(RECT, &rect));
        assert!(read(&stream[..], |_| {}).await.is_err());
    }

    #[tokio::test]
    async fn requires_hello_first() {
        let stream = record(FLUSH, &[]);
        assert!(read(&stream[..], |_| {}).await.is_err());
    }
}
