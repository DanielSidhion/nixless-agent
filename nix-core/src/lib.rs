/// https://github.com/NixOS/nix/blob/c0b6907ccdaf3d3911cfdb2ff2d000e1683997c7/src/libutil/hash.cc#L90
/// To go from nix32 to u8, follow this: https://github.com/NixOS/nix/blob/c0b6907ccdaf3d3911cfdb2ff2d000e1683997c7/src/libutil/hash.cc#L231
pub fn to_nix32(slice: &[u8]) -> String {
    let alphabet = "0123456789abcdfghijklmnpqrsvwxyz";
    let b32len = (slice.len() * 8 - 1) / 5 + 1;

    let mut res = String::with_capacity(b32len);

    for n in (0..b32len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let c = ((slice[i] >> j) as usize)
            | (if i >= slice.len() - 1 {
                0
            } else {
                (slice[i + 1] as usize) << (8 - j)
            });
        let c_i = c & 0x1f;

        res.push(alphabet.chars().nth(c_i).unwrap());
    }

    res
}
