/// A key's digits, in the order their bytes sort.
const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const MAX_LEN: usize = 256;

/// Where a task sits in its list: tasks are listed by key, keys sort as their bytes do, and
/// there is always room for a new key between two others, so moving a task changes only its
/// own key.
///
/// A key is 1 to 256 ASCII letters and digits, the last of them never `0`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrderKey(String);

impl OrderKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A key after `lo` and before `hi`, where `None` is that end of the list. It is `None`
    /// unless `lo` sorts before `hi`, or when the only keys between them are too long.
    #[must_use]
    pub fn between(lo: Option<&Self>, hi: Option<&Self>) -> Option<Self> {
        if let (Some(lo), Some(hi)) = (lo, hi)
            && lo >= hi
        {
            return None;
        }
        let ends = (lo.is_none(), hi.is_none());
        let mut lo = lo.into_iter().flat_map(Self::digits);
        let mut hi = hi.map(Self::digits);
        let mut key = String::new();
        for _ in 0..MAX_LEN {
            let low = lo.next().unwrap_or(0);
            let high = hi.as_mut().and_then(Iterator::next);
            let above = high.unwrap_or(DIGITS.len());
            if above > low + 1 {
                let free = match ends {
                    (true, false) => above - 1,
                    (false, true) => low + 1,
                    _ => low + (above - low) / 2,
                };
                key.push(char::from(*DIGITS.get(free)?));
                return Some(Self(key));
            }
            key.push(char::from(*DIGITS.get(low)?));
            // Past a digit where `hi` is the greater, any further digits sort before it.
            if above > low {
                hi = None;
            }
        }
        None
    }

    fn digits(&self) -> impl Iterator<Item = usize> {
        self.0.bytes().filter_map(digit)
    }
}

fn digit(byte: u8) -> Option<usize> {
    DIGITS.iter().position(|&digit| digit == byte)
}

impl TryFrom<String> for OrderKey {
    type Error = String;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        let digits = key.bytes().all(|byte| digit(byte).is_some());
        if digits && (1..=MAX_LEN).contains(&key.len()) && !key.ends_with('0') {
            Ok(Self(key))
        } else {
            Err(key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> OrderKey {
        OrderKey::try_from(text.to_owned()).unwrap()
    }

    fn between(lo: &str, hi: &str) -> Option<String> {
        let end = |text: &str| (!text.is_empty()).then(|| key(text));
        OrderKey::between(end(lo).as_ref(), end(hi).as_ref()).map(|key| key.as_str().to_owned())
    }

    // The longest key after `moves` moves into one gap, each below or above the last new key.
    fn longest(moves: usize, below: impl Fn(usize) -> bool) -> usize {
        let (mut lo, mut hi) = (key("A"), key("B"));
        let mut longest = 0;
        for turn in 0..moves {
            let new = OrderKey::between(Some(&lo), Some(&hi)).unwrap();
            assert!(lo < new && new < hi, "{lo:?} {new:?} {hi:?}");
            longest = longest.max(new.as_str().len());
            (lo, hi) = if below(turn) { (lo, new) } else { (new, hi) };
        }
        longest
    }

    #[test]
    fn digits_are_in_byte_order() {
        assert!(DIGITS.is_sorted_by(|one, next| one < next));
    }

    #[test]
    fn keys_are_letters_and_digits_that_dont_end_in_zero() {
        let longest = "z".repeat(MAX_LEN);
        for good in ["V", "0V", "a0b", "Zz9", longest.as_str()] {
            assert_eq!(key(good).as_str(), good);
        }
        let too_long = "z".repeat(MAX_LEN + 1);
        for bad in ["", "0", "V0", "a b", "a-b", "é", "V\n", too_long.as_str()] {
            assert_eq!(OrderKey::try_from(bad.to_owned()), Err(bad.to_owned()));
        }
    }

    #[test]
    fn keys_sort_as_their_bytes_do() {
        let mut keys = ["b", "B", "0V", "B1", "9", "Az"].map(key);
        keys.sort();

        assert_eq!(
            keys.each_ref().map(OrderKey::as_str),
            ["0V", "9", "Az", "B", "B1", "b"]
        );
    }

    #[test]
    fn the_first_key_is_in_the_middle() {
        assert_eq!(between("", "").as_deref(), Some("V"));
    }

    #[test]
    fn a_key_at_the_end_takes_the_next_digit_and_grows_when_none_is_left() {
        assert_eq!(between("V", "").as_deref(), Some("W"));
        assert_eq!(between("y", "").as_deref(), Some("z"));
        assert_eq!(between("z", "").as_deref(), Some("z1"));
        assert_eq!(between("zz", "").as_deref(), Some("zz1"));
        assert_eq!(between("zV", "").as_deref(), Some("zW"));
    }

    #[test]
    fn a_key_at_the_start_takes_the_digit_before_and_grows_when_none_is_left() {
        assert_eq!(between("", "V").as_deref(), Some("U"));
        assert_eq!(between("", "2").as_deref(), Some("1"));
        assert_eq!(between("", "1").as_deref(), Some("0z"));
        assert_eq!(between("", "0V").as_deref(), Some("0U"));
        assert_eq!(between("", "01").as_deref(), Some("00z"));
        assert_eq!(between("", "001").as_deref(), Some("000z"));
    }

    #[test]
    fn a_key_between_two_halves_the_gap() {
        assert_eq!(between("A", "C").as_deref(), Some("B"));
        assert_eq!(between("A", "z").as_deref(), Some("Z"));
        assert_eq!(between("1", "4").as_deref(), Some("2"));
        assert_eq!(between("VA", "VC").as_deref(), Some("VB"));
        assert_eq!(between("V1", "VCz").as_deref(), Some("V6"));
    }

    #[test]
    fn neighbours_get_a_longer_key_between_them() {
        assert_eq!(between("A", "B").as_deref(), Some("AV"));
        assert_eq!(between("Az", "B").as_deref(), Some("AzV"));
        assert_eq!(between("A5", "B").as_deref(), Some("AX"));
        assert_eq!(between("A", "B5").as_deref(), Some("AV"));
        assert_eq!(between("V", "VV").as_deref(), Some("VF"));
        assert_eq!(between("V", "V1").as_deref(), Some("V0V"));
        assert_eq!(between("V", "V01").as_deref(), Some("V00V"));
        assert_eq!(between("V", "V0V").as_deref(), Some("V0F"));
        assert_eq!(between("VU", "VV").as_deref(), Some("VUV"));
    }

    #[test]
    fn keys_out_of_order_have_nothing_between_them() {
        assert_eq!(between("B", "A"), None);
        assert_eq!(between("B", "B"), None);
        assert_eq!(between("B1", "B"), None);
    }

    #[test]
    fn a_key_that_would_be_too_long_is_refused() {
        let room = "z".repeat(MAX_LEN - 1);
        let full = "z".repeat(MAX_LEN);

        assert_eq!(between(&room, ""), Some(format!("{room}1")));
        assert_eq!(between(&full, ""), None);
    }

    #[test]
    fn every_new_key_sorts_between_its_neighbours() {
        let mut keys = vec![OrderKey::between(None, None).unwrap()];
        let mut seed: usize = 7;
        for _ in 0..2000 {
            seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648;
            let at = seed % (keys.len() + 1);
            let lo = at.checked_sub(1).and_then(|before| keys.get(before));
            let new = OrderKey::between(lo, keys.get(at)).unwrap();
            assert_eq!(
                OrderKey::try_from(new.as_str().to_owned()).as_ref(),
                Ok(&new)
            );
            keys.insert(at, new);
        }

        assert!(keys.is_sorted_by(|one, next| one < next));
        assert!(keys.iter().all(|key| key.as_str().len() <= 6), "{keys:?}");
    }

    #[test]
    fn keys_stay_short_at_the_ends_of_the_list() {
        let grow = |next: fn(&OrderKey) -> Option<OrderKey>| {
            let mut key = OrderKey::between(None, None).unwrap();
            for _ in 0..1000 {
                key = next(&key).unwrap();
            }
            key.as_str().len()
        };

        assert_eq!(grow(|last| OrderKey::between(Some(last), None)), 17);
        assert_eq!(grow(|first| OrderKey::between(None, Some(first))), 17);
    }

    #[test]
    fn keys_stay_short_under_moves_into_the_same_gap() {
        let below = longest(600, |_| true);
        let above = longest(600, |_| false);
        let both = longest(600, |turn| turn % 2 == 0);

        assert_eq!((below, above, both), (121, 101, 101));
    }
}
