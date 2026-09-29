//! The contract with Blish HUD.
//!
//! Blish HUD and this library talk through three channels, all defined here:
//!
//! - a 32-byte **shared memory header** Blish creates, holding the game's size (written by
//!   us), the two shared texture handles (written by Blish) and a flag saying whether the
//!   cursor is over one of Blish's controls
//! - named **kernel objects**: an event we signal after writing a new size, and a mutex Blish
//!   holds for as long as it is running
//! - **UDP datagrams** carrying the game's mouse input
//!
//! Everything here must stay byte-for-byte in sync with `ExternalDirectxOverlay.cs`.

/// Name of the shared memory mapping Blish creates for the [`Header`]. It lives in the session's
/// local namespace, unlike the kernel objects below.
pub const HEADER_MAPPING_NAME: &str = "BlishHUD_Header";

/// Manual-reset event signalled after new game dimensions are written to the header. Blish
/// recreates its textures at the new size in response.
pub const RESIZE_EVENT_NAME: &str = "Global\\BlishHUD_ResizeEvent";

/// Mutex Blish HUD holds for as long as it is running. If it cannot be opened, Blish is gone.
pub const ALIVE_MUTEX_NAME: &str = "Global\\blish_isalive_mutex";

/// Where mouse input is sent.
pub const INPUT_ADDRESS: &str = "127.0.0.1:49152";

/// Size of the shared memory header, in bytes.
pub const HEADER_SIZE: usize = 32;

/// Byte offsets of the header's fields. All values are little-endian.
mod offset {
    pub const WIDTH: usize = 0;
    pub const HEIGHT: usize = 4;
    pub const NEXT_TEXTURE: usize = 8;
    pub const TEXTURE_0: usize = 12;
    pub const TEXTURE_1: usize = 20;
    pub const BLOCK_MOUSE: usize = 28;
}

/// Offset of the dimensions within the header; see [`encode_dimensions`].
pub const DIMENSIONS_OFFSET: usize = offset::WIDTH;

/// A decoded copy of the shared memory header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    /// Game width in pixels, as last written by us.
    pub width: u32,
    /// Game height in pixels, as last written by us.
    pub height: u32,
    /// Which of the two textures Blish will render into *next*. See
    /// [`completed_texture`](Self::completed_texture).
    pub next_texture: u32,
    /// Shared handles of Blish's two render textures. Zero until Blish has created them.
    pub textures: [u64; 2],
    /// Whether the cursor is over one of Blish's controls, meaning clicks belong to Blish.
    pub block_mouse: bool,
}

impl Header {
    /// Decodes a header from its raw bytes.
    pub fn decode(bytes: &[u8; HEADER_SIZE]) -> Self {
        Self {
            width: read_u32(bytes, offset::WIDTH),
            height: read_u32(bytes, offset::HEIGHT),
            next_texture: read_u32(bytes, offset::NEXT_TEXTURE),
            textures: [
                read_u64(bytes, offset::TEXTURE_0),
                read_u64(bytes, offset::TEXTURE_1),
            ],
            block_mouse: read_u32(bytes, offset::BLOCK_MOUSE) != 0,
        }
    }

    /// Whether Blish has published both of its textures yet.
    pub fn has_textures(&self) -> bool {
        self.textures.iter().all(|&handle| handle != 0)
    }

    /// Index of the texture holding the most recently *completed* frame.
    ///
    /// Blish renders into one texture, then flips the index and publishes it — so the
    /// published index names the texture about to be overwritten, and the finished frame is the
    /// other one. Drawing the published index would sample a frame while it is being rendered.
    pub fn completed_texture(&self) -> usize {
        (self.next_texture as usize & 1) ^ 1
    }
}

/// Encodes game dimensions for writing at [`DIMENSIONS_OFFSET`].
pub fn encode_dimensions(width: u32, height: u32) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&width.to_le_bytes());
    bytes[4..].copy_from_slice(&height.to_le_bytes());
    bytes
}

/// Win32 mouse message ids.
///
/// These go over the wire unchanged: Blish's `MouseEventType` enum is defined with the same
/// values, so the C# side casts them straight across.
pub mod wm {
    /// `WM_MOUSEMOVE`
    pub const MOUSEMOVE: u32 = 0x0200;
    /// `WM_LBUTTONDOWN`
    pub const LBUTTONDOWN: u32 = 0x0201;
    /// `WM_LBUTTONUP`
    pub const LBUTTONUP: u32 = 0x0202;
    /// `WM_RBUTTONDOWN`
    pub const RBUTTONDOWN: u32 = 0x0204;
    /// `WM_RBUTTONUP`
    pub const RBUTTONUP: u32 = 0x0205;
    /// `WM_MBUTTONDOWN`
    pub const MBUTTONDOWN: u32 = 0x0207;
    /// `WM_MBUTTONUP`
    pub const MBUTTONUP: u32 = 0x0208;
    /// `WM_MOUSEWHEEL`
    pub const MOUSEWHEEL: u32 = 0x020A;
}

/// One mouse message, as sent to Blish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MousePacket {
    /// The Win32 message id; see [`wm`].
    pub message: u32,
    /// Cursor x, in client coordinates (screen coordinates for `WM_MOUSEWHEEL`).
    pub x: i32,
    /// Cursor y, in client coordinates (screen coordinates for `WM_MOUSEWHEEL`).
    pub y: i32,
    /// The message's original `wParam`. Its low word holds the `MK_*` button flags, from which
    /// Blish rebuilds button state on every packet; for the wheel, the high word is the delta.
    pub data: i32,
}

impl MousePacket {
    /// Size of an encoded packet, in bytes.
    pub const SIZE: usize = 16;

    /// Encodes the packet for sending.
    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.message.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.x.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.y.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.data.to_le_bytes());
        bytes
    }
}

/// Unpacks the signed coordinates of a mouse message's `lParam`, as `GET_X_LPARAM` and
/// `GET_Y_LPARAM` do. They are signed because the cursor can be left of or above the window.
pub fn point_from_lparam(lparam: isize) -> (i32, i32) {
    let packed = lparam as u32;
    let x = (packed & 0xFFFF) as u16 as i16;
    let y = (packed >> 16) as u16 as i16;
    (i32::from(x), i32::from(y))
}

/// Unpacks the client size carried by a `WM_SIZE` message's `lParam`.
pub fn size_from_lparam(lparam: isize) -> (u32, u32) {
    let packed = lparam as u32;
    (packed & 0xFFFF, packed >> 16)
}

fn read_u32(bytes: &[u8; HEADER_SIZE], at: usize) -> u32 {
    let mut field = [0; 4];
    field.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(field)
}

fn read_u64(bytes: &[u8; HEADER_SIZE], at: usize) -> u64 {
    let mut field = [0; 8];
    field.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(
        width: u32,
        height: u32,
        next: u32,
        textures: [u64; 2],
        block: u32,
    ) -> [u8; HEADER_SIZE] {
        let mut bytes = [0; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&width.to_le_bytes());
        bytes[4..8].copy_from_slice(&height.to_le_bytes());
        bytes[8..12].copy_from_slice(&next.to_le_bytes());
        bytes[12..20].copy_from_slice(&textures[0].to_le_bytes());
        bytes[20..28].copy_from_slice(&textures[1].to_le_bytes());
        bytes[28..32].copy_from_slice(&block.to_le_bytes());
        bytes
    }

    #[test]
    fn decodes_every_field_at_its_offset() {
        let bytes = header_bytes(2560, 1440, 1, [0xAABB, 0xCCDD_0000_1122], 1);

        assert_eq!(
            Header::decode(&bytes),
            Header {
                width: 2560,
                height: 1440,
                next_texture: 1,
                textures: [0xAABB, 0xCCDD_0000_1122],
                block_mouse: true,
            }
        );
    }

    #[test]
    fn any_nonzero_block_value_means_blocked() {
        let bytes = header_bytes(0, 0, 0, [0, 0], 7);
        assert!(Header::decode(&bytes).block_mouse);
    }

    #[test]
    fn textures_are_only_available_once_both_handles_are_set() {
        let one = Header::decode(&header_bytes(0, 0, 0, [5, 0], 0));
        let both = Header::decode(&header_bytes(0, 0, 0, [5, 6], 0));

        assert!(!one.has_textures());
        assert!(both.has_textures());
    }

    #[test]
    fn completed_texture_is_the_one_not_about_to_be_written() {
        let header = |next| Header {
            next_texture: next,
            ..Header::default()
        };

        assert_eq!(header(0).completed_texture(), 1);
        assert_eq!(header(1).completed_texture(), 0);
    }

    #[test]
    fn completed_texture_stays_in_bounds_for_garbage_indices() {
        let header = Header {
            next_texture: u32::MAX,
            ..Header::default()
        };
        assert!(header.completed_texture() < 2);
    }

    #[test]
    fn dimensions_round_trip_through_the_header() {
        let mut bytes = [0; HEADER_SIZE];
        bytes[DIMENSIONS_OFFSET..DIMENSIONS_OFFSET + 8]
            .copy_from_slice(&encode_dimensions(1920, 1080));

        let header = Header::decode(&bytes);
        assert_eq!((header.width, header.height), (1920, 1080));
    }

    #[test]
    fn mouse_packet_layout_matches_the_csharp_reader() {
        let packet = MousePacket {
            message: wm::MOUSEWHEEL,
            x: -3,
            y: 1080,
            data: 0x0078_0000,
        };
        let bytes = packet.encode();

        assert_eq!(bytes.len(), MousePacket::SIZE);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 0x020A);
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), -3);
        assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()), 1080);
        assert_eq!(
            i32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            0x0078_0000
        );
    }

    #[test]
    fn lparam_points_are_sign_extended() {
        // x = -5 (0xFFFB), y = 300 (0x012C)
        let lparam = 0x012C_FFFB_isize;
        assert_eq!(point_from_lparam(lparam), (-5, 300));
    }

    #[test]
    fn lparam_points_ignore_the_upper_half_of_64_bit_values() {
        let lparam = 0x7FFF_0000_0064_0032_isize;
        assert_eq!(point_from_lparam(lparam), (0x32, 0x64));
    }

    #[test]
    fn wm_size_is_unsigned_width_then_height() {
        let lparam = (1080_isize << 16) | 1920;
        assert_eq!(size_from_lparam(lparam), (1920, 1080));
    }
}
