//! Lexicographic fractional keys. Digits are 1..=254 and a generated key never ends in 1.
//! That last rule leaves space even between a key and an extension of it.

/// A nonempty sequence of digits in 1..=254 ending in a non-minimal digit.
pub fn valid(key: &[u8]) -> bool {
    matches!(key.last(), Some(2..=254)) && key.iter().all(|digit| (1..=254).contains(digit))
}

/// A key strictly inside the open interval, or `None` for malformed or unordered bounds.
/// Use `None` for either open end. Concurrent inserts with the same bounds get the same key.
pub fn try_between(a: Option<&[u8]>, b: Option<&[u8]>) -> Option<Vec<u8>> {
    if a.is_some_and(|key| !valid(key)) || b.is_some_and(|key| !valid(key)) { return None }
    let lower = a.unwrap_or_default();
    let Some(upper) = b else { return Some(after(lower)) };
    if lower >= upper { return None }
    let shared = lower.iter().zip(upper).take_while(|(l, r)| l == r).count();
    let mut key = lower[..shared].to_vec();
    if let Some(&left) = lower.get(shared) {
        let gap = upper[shared] - left;
        if gap > 1 {
            key.push(left + gap / 2);
        } else {
            key.push(left);
            key.extend(after(&lower[shared + 1..]));
        }
    } else {
        // The lower bound is a prefix. Stay below the first non-minimal upper digit.
        let tail = &upper[shared..];
        let ones = tail.iter().take_while(|&&digit| digit == 1).count();
        key.extend_from_slice(&tail[..ones]);
        let digit = tail[ones];
        if digit == 2 {
            key.extend([1, 128]);
        } else {
            key.push(digit.div_ceil(2));
        }
    }
    Some(key)
}

fn after(lower: &[u8]) -> Vec<u8> {
    if lower.is_empty() {
        return vec![128];
    }
    let mut key = lower.to_vec();
    if let Some(index) = key.iter().rposition(|&digit| digit < 254) {
        key[index] += 1;
        key[index + 1..].fill(2);
    } else {
        // Two more digits allow a run of appends to count through the whole new suffix.
        key.extend([2, 2]);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_inserts_stay_between_neighbours() {
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut seed = 7u32;
        for i in 0..2000 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let at = match i % 3 {
                0 => 0,
                1 => keys.len(),
                _ => seed as usize % (keys.len() + 1),
            };
            let left = at.checked_sub(1).map(|p| keys[p].as_slice());
            let right = keys.get(at).map(Vec::as_slice);
            let key = try_between(left, right).unwrap();
            assert!(left.is_none_or(|l| l < key.as_slice()), "insert {i} exceeds its lower bound");
            assert!(right.is_none_or(|r| key.as_slice() < r), "insert {i} precedes its upper bound");
            assert!(key.iter().all(|d| (1..=254).contains(d)) && key.last() != Some(&1), "insert {i} leaves a dense interval");
            keys.insert(at, key);
        }
    }

    #[test]
    fn appends_stay_short() {
        let mut key = try_between(None, None).unwrap();
        for _ in 1..10_000 {
            let next = try_between(Some(&key), None).unwrap();
            assert!(next > key && next.len() < 36, "each append advances with a short key");
            key = next;
        }
    }

    #[test]
    fn adjacent_digits_and_prefixes_leave_room() {
        for (left, right) in [(&[8, 254][..], &[9][..]), (&[8], &[8, 1, 1, 2])] {
            let key = try_between(Some(left), Some(right)).unwrap();
            assert!(left < key.as_slice() && key.as_slice() < right, "room between {left:?} and {right:?}");
        }
    }

    #[test]
    fn arbitrary_bounds_return_exactly_the_valid_intervals() {
        let mut keys = vec![vec![], vec![0], vec![1], vec![255], vec![255; 3], vec![254; 2], vec![8, 1, 1]];
        let mut seed = 11u32;
        let mut byte = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        for _ in 0..16 {
            for len in 0..=6 { keys.push((0..len).map(|_| byte()).collect()); }
        }
        let bounds = || std::iter::once(None).chain(keys.iter().map(|k| Some(k.as_slice())));
        let grammar = |k: &[u8]| !k.is_empty() && k.last() != Some(&1) && k.iter().all(|&d| d != 0 && d != 255);
        assert!(try_between(None, None).is_some(), "AddItem with no top key is never refused");
        for k in keys.iter().filter(|k| grammar(k)) {
            assert!(try_between(Some(k), None).is_some(), "AddItem above a valid top key is never refused: {k:?}");
        }
        for a in bounds() {
            for b in bounds() {
                let expected = a.is_none_or(grammar) && b.is_none_or(grammar) && a.zip(b).is_none_or(|(l, r)| l < r);
                let key = try_between(a, b);
                assert_eq!(key.is_some(), expected, "interval {a:?} to {b:?}");
                if let Some(key) = key {
                    assert!(grammar(&key) && a.is_none_or(|l| l < key.as_slice()) && b.is_none_or(|r| key.as_slice() < r),
                        "key {key:?} in interval {a:?} to {b:?}");
                }
            }
        }
    }
}
