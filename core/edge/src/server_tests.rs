use super::*;
use proptest::prelude::*;

fn tagged_forward(tag: String) -> ForwardHandler {
    Arc::new(move |_method: String, _payload: Vec<u8>| {
        let tag = tag.clone();
        Box::pin(async move { Ok(tag.into_bytes()) }) as BoxFuture<'static, HandlerResult>
    })
}

fn dispatch_with(prefixes: &[String]) -> Dispatch {
    Dispatch {
        handlers: HashMap::new(),
        id_handlers: HashMap::new(),
        prefixes: prefixes.iter().map(|p| (p.clone(), tagged_forward(p.clone()))).collect(),
    }
}

// --- Property test (port of Go's TestPropPrefixLongestMatch in edge/prop_test.go) ---
//
// For any set of distinct registered prefixes and any method string,
// `Dispatch::longest_prefix` matches iff some registered prefix is a
// `str::starts_with` of the method, and when it matches it selects the LONGEST
// such prefix. An oracle loop computes the expected result independently; each
// handler is tagged with its own prefix so the winner is identifiable.
proptest! {
    #[test]
    fn prop_prefix_longest_match(
        prefixes in proptest::collection::hash_set("[a-z]{1,5}\\.", 0..8),
        use_registered in any::<bool>(),
        chosen_idx in 0usize..8,
        suffix in "[a-z.]{0,6}",
        random_method in "[a-z.]{0,12}",
    ) {
        let prefixes: Vec<String> = prefixes.into_iter().collect();
        let dispatch = dispatch_with(&prefixes);

        let method = if !prefixes.is_empty() && use_registered {
            let chosen = &prefixes[chosen_idx % prefixes.len()];
            format!("{chosen}{suffix}")
        } else {
            random_method
        };

        // Oracle: the longest registered prefix that `method` starts with.
        let mut best_len: isize = -1;
        let mut best_tag: Option<&str> = None;
        for p in &prefixes {
            if method.starts_with(p.as_str()) && p.len() as isize > best_len {
                best_len = p.len() as isize;
                best_tag = Some(p.as_str());
            }
        }
        let want_ok = best_len >= 0;

        let got = dispatch.longest_prefix(&method);
        prop_assert_eq!(got.is_some(), want_ok);

        if let Some(fwd) = got {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let got_tag = rt.block_on(fwd(method.clone(), Vec::new())).unwrap();
            prop_assert_eq!(String::from_utf8(got_tag).unwrap(), best_tag.unwrap().to_string());
        }
    }
}
