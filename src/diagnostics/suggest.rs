//! Name ranking for “did you mean …?” hints.

/// Damerau-ish Levenshtein distance (insert/delete/substitute; adjacent transpose).
pub fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            let mut best = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
            if i > 0 && j > 0 && ca == &b[j - 1] && cb == &a[i - 1] {
                best = best.min(prev[j - 1] + cost);
            }
            cur[j + 1] = best;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Candidates ranked by case-insensitive distance. Closest first.
pub fn rank<I, S>(target: &str, candidates: I) -> Vec<(usize, String)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let lower = target.to_ascii_lowercase();
    let mut ranked: Vec<(usize, String)> = candidates
        .into_iter()
        .map(|candidate| {
            let candidate = candidate.as_ref();
            (
                distance(&lower, &candidate.to_ascii_lowercase()),
                candidate.to_string(),
            )
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    ranked.dedup_by(|a, b| a.1.eq_ignore_ascii_case(&b.1));
    ranked
}

/// Best suggestion when the name is a near miss (distance ≤ 2, or case-only).
pub fn suggest_one<I, S>(target: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let ranked = rank(target, candidates);
    let (dist, name) = ranked.into_iter().next()?;
    if dist == 0 {
        if !name.eq_ignore_ascii_case(target) {
            return Some(name);
        }
        if name != target {
            return Some(name);
        }
        return None;
    }
    let min_len = target.chars().count().max(name.chars().count());
    if dist == 1 || (dist <= 2 && min_len >= 4) {
        Some(name)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_case_variant() {
        assert_eq!(
            suggest_one("outext", ["OutText", "OutInt", "OutImage"]).as_deref(),
            Some("OutText")
        );
    }

    #[test]
    fn suggests_one_edit() {
        assert_eq!(
            suggest_one("OutTxt", ["OutText", "OutInt"]).as_deref(),
            Some("OutText")
        );
    }

    #[test]
    fn ignores_unrelated() {
        assert_eq!(suggest_one("zzzz", ["OutText", "hold"]), None);
    }
}
