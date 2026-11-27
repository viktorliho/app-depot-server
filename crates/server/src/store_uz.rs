// Classic Unreal redirect format as used on the original UT GOTY disc.
use std::io;
const LIMIT: usize = 128 << 20;
fn invalid() -> io::Error {
    io::Error::other("Invalid Unreal UZ stream")
}
fn word(bytes: &[u8]) -> io::Result<usize> {
    Ok(u32::from_le_bytes(bytes.get(..4).ok_or_else(invalid)?.try_into().unwrap()) as usize)
}
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}
impl Bits<'_> {
    fn bit(&mut self) -> io::Result<usize> {
        let b = self.data.get(self.pos / 8).ok_or_else(invalid)?;
        let bit = (b >> (self.pos % 8)) & 1;
        self.pos += 1;
        Ok(bit.into())
    }
}
enum Node {
    Symbol(u8),
    Branch(usize, usize),
}
fn tree(
    bits: &mut Bits<'_>,
    nodes: &mut Vec<Node>,
    seen: &mut [bool; 256],
    depth: usize,
) -> io::Result<usize> {
    if depth > 255 || nodes.len() >= 511 {
        return Err(invalid());
    }
    let node = if bits.bit()? == 1 {
        Node::Branch(
            tree(bits, nodes, seen, depth + 1)?,
            tree(bits, nodes, seen, depth + 1)?,
        )
    } else {
        let mut value = 0;
        for i in 0..8 {
            value |= bits.bit()? << i
        }
        if std::mem::replace(&mut seen[value], true) {
            return Err(invalid());
        }
        Node::Symbol(value as u8)
    };
    nodes.push(node);
    Ok(nodes.len() - 1)
}
fn huffman(input: &[u8]) -> io::Result<Vec<u8>> {
    let n = word(input)?;
    if !(1..=LIMIT).contains(&n) {
        return Err(invalid());
    }
    let mut bits = Bits {
        data: &input[4..],
        pos: 0,
    };
    let mut nodes = Vec::new();
    let root = tree(&mut bits, &mut nodes, &mut [false; 256], 0)?;
    let mut output = Vec::with_capacity(n);
    while output.len() < n {
        let mut index = root;
        loop {
            match nodes[index] {
                Node::Symbol(b) => {
                    output.push(b);
                    break;
                }
                Node::Branch(l, r) => index = if bits.bit()? == 0 { l } else { r },
            }
        }
    }
    if bits.data.len() * 8 - bits.pos >= 8 {
        return Err(invalid());
    }
    while bits.pos < bits.data.len() * 8 {
        if bits.bit()? != 0 {
            return Err(invalid());
        }
    }
    Ok(output)
}
fn rle(data: Vec<u8>) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut iter = data.into_iter();
    let mut last = None;
    let mut run = 0;
    while let Some(b) = iter.next() {
        if last != Some(b) {
            run = 0;
            last = Some(b)
        }
        run += 1;
        let copies = if run == 5 {
            run = 0;
            usize::from(iter.next().filter(|&n| n >= 5).ok_or_else(invalid)?) - 4
        } else {
            1
        };
        if out.len() + copies > LIMIT {
            return Err(invalid());
        }
        out.resize(out.len() + copies, b);
    }
    Ok(out)
}
pub fn decode(input: &[u8], name: &str) -> io::Result<Vec<u8>> {
    if input.len() > 64 << 20 {
        return Err(invalid());
    }
    let signature = word(input)?;
    if !matches!(signature, 1234 | 5678) {
        return Err(invalid());
    }
    let first = *input.get(4).ok_or_else(invalid)?;
    if first & 128 != 0 {
        return Err(invalid());
    }
    let mut length = (first & 63) as usize;
    let mut offset = 5;
    if first & 64 != 0 {
        let next = *input.get(offset).ok_or_else(invalid)?;
        if next > 4 {
            return Err(invalid());
        }
        length += usize::from(next) << 6;
        offset += 1
    }
    if !(2..=256).contains(&length)
        || input.get(offset..offset + length - 1) != Some(name.as_bytes())
        || input.get(offset + length - 1) != Some(&0)
    {
        return Err(invalid());
    }
    let mut data = huffman(&input[offset + length..])?;
    if signature == 5678 {
        data = rle(data)?;
    }
    let mut alphabet: [u8; 256] = std::array::from_fn(|i| i as u8);
    for b in &mut data {
        let rank = *b as usize;
        *b = alphabet[rank];
        alphabet[..=rank].rotate_right(1);
    }
    let mut out = Vec::new();
    let mut rest = data.as_slice();
    while !rest.is_empty() {
        let n = word(rest)?;
        let mut cursor = word(&rest[4..])?;
        let last = word(&rest[8..])?;
        if n == 0 || n > 256 << 10 || cursor > n || last > n || n + out.len() > LIMIT {
            return Err(invalid());
        }
        let block = rest.get(12..13 + n).ok_or_else(invalid)?;
        rest = &rest[13 + n..];
        let key = |i: usize| if i == last { 256 } else { block[i] as usize };
        let mut offsets = [0usize; 257];
        for i in 0..=n {
            offsets[key(i)] += 1;
        }
        let mut total = 0;
        for v in &mut offsets {
            let count = *v;
            *v = total;
            total += count
        }
        let mut links = vec![0; n + 1];
        for i in 0..=n {
            let k = key(i);
            links[offsets[k]] = i;
            offsets[k] += 1;
        }
        for _ in 0..n {
            out.push(block[cursor]);
            cursor = links[cursor];
        }
    }
    rle(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_uz_validates_filename_and_truncation() {
        let packed=hex::decode("2e1600000a53746172742e756e7200240000004fc0456c0d28b8031469385918b2ceb405706fe09243b73a7902f00bbfe4979916aa3d1c69e2c115e797670a").unwrap();
        assert_eq!(
            decode(&packed, "Start.unr").unwrap(),
            b"synthetic map dataaaaaaaaaa"
        );
        assert!(decode(&packed, "Wrong.unr").is_err());
        assert!(decode(&packed[..packed.len() - 1], "Start.unr").is_err());
    }
}
