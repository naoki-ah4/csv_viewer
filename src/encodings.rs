//! 文字コードの手動切り替えで選べる項目。画面には依存しない。

use encoding_rs::Encoding;

/// 切り替えメニューの1項目。`encoding` が `None` なら「自動判定」。
pub struct Choice {
    pub label: &'static str,
    pub encoding: Option<&'static Encoding>,
}

/// メニューに並べる項目。先頭が「自動判定」。
pub fn choices() -> [Choice; 7] {
    [
        Choice {
            label: "自動判定",
            encoding: None,
        },
        Choice {
            label: "UTF-8",
            encoding: Some(encoding_rs::UTF_8),
        },
        Choice {
            label: "Shift_JIS",
            encoding: Some(encoding_rs::SHIFT_JIS),
        },
        Choice {
            label: "EUC-JP",
            encoding: Some(encoding_rs::EUC_JP),
        },
        Choice {
            label: "UTF-16LE",
            encoding: Some(encoding_rs::UTF_16LE),
        },
        Choice {
            label: "UTF-16BE",
            encoding: Some(encoding_rs::UTF_16BE),
        },
        Choice {
            label: "ISO-2022-JP",
            encoding: Some(encoding_rs::ISO_2022_JP),
        },
    ]
}

/// いま選ばれている項目か。`forced` が `None` なら、自動判定が選ばれている。
pub fn is_active(choice: &Choice, forced: Option<&'static Encoding>) -> bool {
    choice.encoding == forced
}

/// ステータスバーに出す文字コードの表示。手動で指定しているときは、そう分かるようにする。
pub fn status_label(current: &'static Encoding, forced: Option<&'static Encoding>) -> String {
    if forced.is_some() {
        format!("{}（指定）", current.name())
    } else {
        current.name().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_choice_is_automatic_detection() {
        let choices = choices();
        assert_eq!(choices[0].label, "自動判定");
        assert!(choices[0].encoding.is_none());
        assert!(choices[1..].iter().all(|c| c.encoding.is_some()));
    }

    #[test]
    fn labels_and_encodings_are_unique() {
        let choices = choices();
        for (i, a) in choices.iter().enumerate() {
            for b in &choices[i + 1..] {
                assert_ne!(a.label, b.label);
                assert_ne!(a.encoding, b.encoding);
            }
        }
    }

    #[test]
    fn exactly_one_choice_is_active() {
        for forced in [
            None,
            Some(encoding_rs::SHIFT_JIS),
            Some(encoding_rs::UTF_16BE),
            Some(encoding_rs::ISO_2022_JP),
        ] {
            let active = choices().iter().filter(|c| is_active(c, forced)).count();
            assert_eq!(active, 1, "{forced:?}");
        }
        // 一覧にない文字コードが指定された場合は、どれも選ばれていない。
        let unlisted = Some(encoding_rs::GBK);
        assert_eq!(
            choices().iter().filter(|c| is_active(c, unlisted)).count(),
            0
        );
    }

    #[test]
    fn the_status_label_shows_when_the_encoding_was_chosen_by_hand() {
        assert_eq!(status_label(encoding_rs::SHIFT_JIS, None), "Shift_JIS");
        assert_eq!(
            status_label(encoding_rs::SHIFT_JIS, Some(encoding_rs::SHIFT_JIS)),
            "Shift_JIS（指定）"
        );
        assert_eq!(
            status_label(encoding_rs::UTF_8, Some(encoding_rs::UTF_8)),
            "UTF-8（指定）"
        );
    }
}
