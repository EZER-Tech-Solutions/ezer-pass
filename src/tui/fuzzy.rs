//! Fuzzy matching for the TUI's store-wide search.

/// Score `candidate` against a whitespace-separated `query`: every term must
/// appear in it as a case-insensitive subsequence. `None` means no match;
/// higher is better. An empty query matches everything with score 0.
pub fn score(query: &str, candidate: &str) -> Option<i64> {
    let cand: Vec<char> = candidate.to_lowercase().chars().collect();
    let mut total = 0;
    for term in query.split_whitespace() {
        let term: Vec<char> = term.to_lowercase().chars().collect();
        total += score_term(&term, &cand)?;
    }
    Some(total)
}

fn score_term(term: &[char], cand: &[char]) -> Option<i64> {
    // Where the last path segment (the entry's own name) starts.
    let base_start = cand.iter().rposition(|&c| c == '/').map_or(0, |i| i + 1);
    let mut score = 0;
    let mut from = 0;
    let mut prev: Option<usize> = None;
    for &qc in term {
        let i = (from..cand.len()).find(|&i| cand[i] == qc)?;
        score += 1;
        if i == 0 || matches!(cand[i - 1], '/' | '-' | '_' | '.' | ' ') {
            score += 8;
        }
        match prev {
            Some(p) if p + 1 == i => score += 5,
            Some(p) => score -= (i - p - 1).min(5) as i64,
            None => {}
        }
        if i >= base_start {
            score += 2;
        }
        prev = Some(i);
        from = i + 1;
    }
    // Greedy subsequence matching can miss the best alignment; a contiguous
    // substring, especially in the entry's own name, is the strongest signal.
    if contains(cand, term) {
        score += 10;
        if contains(&cand[base_start..], term) {
            score += 10;
        }
    }
    Some(score)
}

fn contains(hay: &[char], needle: &[char]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_required() {
        assert!(score("wb1", "servers/web1").is_some());
        assert!(score("xyz", "servers/web1").is_none());
        assert!(score("1bew", "servers/web1").is_none());
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn case_insensitive() {
        assert!(score("WEB", "servers/web1").is_some());
        assert!(score("web", "Servers/Web1").is_some());
    }

    #[test]
    fn all_terms_must_match() {
        assert!(score("srv web", "servers/web1").is_some());
        assert!(score("srv web", "apple/web").is_none());
    }

    #[test]
    fn name_match_beats_scattered_match() {
        let a = score("web", "servers/web1").unwrap();
        let b = score("web", "work/ebay").unwrap();
        assert!(a > b, "{a} <= {b}");
    }

    #[test]
    fn basename_beats_directory() {
        let a = score("mail", "personal/mail").unwrap();
        let b = score("mail", "mail/personal").unwrap();
        assert!(a > b, "{a} <= {b}");
    }
}
