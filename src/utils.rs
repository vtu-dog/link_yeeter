//! Utility functions used throughout the project.

use std::collections::HashSet;

use linkify::{LinkFinder, LinkKind};
use rand::distr::{Alphanumeric, SampleString};
use url::Url;

/// Returns a random alphanumeric string of the specified length.
pub fn random_string(size: usize) -> String {
    Alphanumeric.sample_string(&mut rand::rng(), size)
}

/// Information about URLs found in a message.
#[derive(Debug, PartialEq, Eq)]
pub enum URLsFound {
    /// No URLs found.
    None,
    /// One URL found.
    One {
        /// The URL found.
        url: String,
        /// Whether the URL is in the allowlist.
        supported: bool,
    },
    /// Multiple URLs found.
    Multiple,
}

/// Reduces a host to its registrable domain using the Public Suffix List:
/// `www.youtube.com` becomes `youtube.com`, `www.bbc.co.uk` becomes `bbc.co.uk`.
///
/// Hosts without a registrable domain (IP addresses, single labels) are
/// returned unchanged.
fn extract_netloc(host: &str) -> &str {
    psl::domain_str(host).unwrap_or(host)
}

/// Parses a message and returns information about URLs found inside it,
/// checking each URL's registrable domain against `allowlist`.
pub fn get_url_info(msg: &str, allowlist: &HashSet<String>) -> URLsFound {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);

    let urls: Vec<Url> = finder
        .links(msg)
        .filter_map(|link| Url::parse(link.as_str()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .collect();

    match urls.len() {
        0 => URLsFound::None,
        1 => {
            let url = &urls[0];
            let netloc = match url.host() {
                Some(url::Host::Domain(domain)) => extract_netloc(domain),
                // IP addresses have no registrable domain, match them verbatim
                Some(_) => url.host_str().unwrap_or_default(),
                None => return URLsFound::None,
            };

            URLsFound::One {
                url: url.to_string(),
                supported: allowlist.contains(netloc),
            }
        }
        _ => URLsFound::Multiple,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod random_string {
        use super::*;

        #[test]
        fn returns_empty_string_for_zero_size() {
            assert_eq!(random_string(0), "");
        }

        #[test]
        fn returns_correct_length() {
            assert_eq!(random_string(10).len(), 10);
            assert_eq!(random_string(100).len(), 100);
        }

        #[test]
        fn contains_only_alphanumeric_chars() {
            let s = random_string(1000);
            assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
        }

        #[test]
        fn generates_different_strings() {
            let s1 = random_string(20);
            let s2 = random_string(20);
            // extremely unlikely to be equal by chance
            assert_ne!(s1, s2);
        }
    }

    mod extract_netloc {
        use super::*;

        #[test]
        fn strips_subdomains_from_simple_tlds() {
            assert_eq!(extract_netloc("www.youtube.com"), "youtube.com");
            assert_eq!(extract_netloc("youtube.com"), "youtube.com");
        }

        #[test]
        fn preserves_multi_part_tlds() {
            assert_eq!(extract_netloc("www.bbc.co.uk"), "bbc.co.uk");
            assert_eq!(extract_netloc("bbc.co.uk"), "bbc.co.uk");
            assert_eq!(extract_netloc("x.com.au"), "x.com.au");
        }

        #[test]
        fn reduces_spoofed_hosts_to_the_actual_domain() {
            assert_eq!(extract_netloc("youtube.com.evil.com"), "evil.com");
        }

        #[test]
        fn passes_through_hosts_without_registrable_domain() {
            assert_eq!(extract_netloc("localhost"), "localhost");
        }
    }

    mod get_url_info {
        use super::*;

        #[test]
        fn returns_none_for_empty_string() {
            assert_eq!(get_url_info("", &HashSet::new()), URLsFound::None);
        }

        #[test]
        fn returns_none_for_plain_text() {
            assert_eq!(
                get_url_info("just some plain text", &HashSet::new()),
                URLsFound::None
            );
        }

        #[test]
        fn rejects_non_http_schemes() {
            assert_eq!(
                get_url_info("ftp://files.example.com", &HashSet::new()),
                URLsFound::None
            );
            assert_eq!(
                get_url_info("mailto:test@example.com", &HashSet::new()),
                URLsFound::None
            );
            assert_eq!(
                get_url_info("not://a-valid-url", &HashSet::new()),
                URLsFound::None
            );
        }

        #[test]
        fn extracts_single_url() {
            let result = get_url_info("check out https://example.com/video", &HashSet::new());
            match result {
                URLsFound::One { url, .. } => {
                    assert_eq!(url, "https://example.com/video");
                }
                _ => panic!("expected URLsFound::One, got {result:?}"),
            }
        }

        #[test]
        fn extracts_url_with_query_params() {
            let result = get_url_info("https://example.com/watch?v=abc123&t=10", &HashSet::new());
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains("v=abc123"));
                    assert!(url.contains("t=10"));
                }
                _ => panic!("expected URLsFound::One, got {result:?}"),
            }
        }

        #[test]
        fn returns_multiple_for_two_urls() {
            assert_eq!(
                get_url_info("https://example.com and https://other.com", &HashSet::new()),
                URLsFound::Multiple
            );
        }

        #[test]
        fn extracts_netloc_without_subdomain() {
            // the URL is preserved verbatim
            // only the allowlist check uses the registrable domain
            let result = get_url_info("https://www.example.com/path", &HashSet::new());
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains("www.example.com"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn matches_allowlist_via_registrable_domain() {
            let allowlist: HashSet<String> = ["bbc.co.uk".to_string()].into();

            for msg in [
                "https://www.bbc.co.uk/video",
                "https://bbc.co.uk/video",
                "https://media.sub.bbc.co.uk/video",
            ] {
                match get_url_info(msg, &allowlist) {
                    URLsFound::One { supported, .. } => {
                        assert!(supported, "{msg} should be supported");
                    }
                    other => panic!("expected URLsFound::One for {msg}, got {other:?}"),
                }
            }
        }

        #[test]
        fn matches_ip_hosts_verbatim() {
            let allowlist: HashSet<String> = ["192.168.1.1".to_string()].into();

            match get_url_info("http://192.168.1.1/video", &allowlist) {
                URLsFound::One { supported, .. } => {
                    assert!(supported, "allowlisted IP host should be supported");
                }
                other => panic!("expected URLsFound::One, got {other:?}"),
            }
        }

        #[test]
        fn rejects_lookalike_subdomain_spoof() {
            let allowlist: HashSet<String> = ["youtube.com".to_string()].into();

            match get_url_info("https://youtube.com.evil.com/video", &allowlist) {
                URLsFound::One { supported, .. } => {
                    assert!(!supported, "spoofed host should not be supported");
                }
                other => panic!("expected URLsFound::One, got {other:?}"),
            }
        }

        #[test]
        fn handles_url_in_middle_of_text() {
            let result = get_url_info(
                "Please download https://example.com/file.mp4 thanks!",
                &HashSet::new(),
            );
            match result {
                URLsFound::One { url, .. } => {
                    assert_eq!(url, "https://example.com/file.mp4");
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_url_with_port() {
            let result = get_url_info("https://example.com:8080/path", &HashSet::new());
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains(":8080"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_url_with_fragment() {
            let result = get_url_info("https://example.com/page#section", &HashSet::new());
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains("#section"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_http_url() {
            let result = get_url_info("http://example.com/video", &HashSet::new());
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.starts_with("http://"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn handles_url_with_encoded_chars() {
            let result = get_url_info("https://example.com/path%20with%20spaces", &HashSet::new());
            match result {
                URLsFound::One { url, .. } => {
                    assert!(url.contains("%20"));
                }
                _ => panic!("expected URLsFound::One"),
            }
        }

        #[test]
        fn returns_multiple_for_three_urls() {
            assert_eq!(
                get_url_info("https://a.com https://b.com https://c.com", &HashSet::new()),
                URLsFound::Multiple
            );
        }
    }
}
