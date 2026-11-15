//! Character sets and collation ids (`information_schema.COLLATIONS` of
//! MySQL 8.4 and MariaDB 11.4; ids up to 255 never conflict between them).

/// Character set of each collation id that fits the handshake's byte.
const COLLATION_CHARSETS: &[(u8, &str)] = &[
    (1, "big5"),
    (2, "latin2"),
    (3, "dec8"),
    (4, "cp850"),
    (5, "latin1"),
    (6, "hp8"),
    (7, "koi8r"),
    (8, "latin1"),
    (9, "latin2"),
    (10, "swe7"),
    (11, "ascii"),
    (12, "ujis"),
    (13, "sjis"),
    (14, "cp1251"),
    (15, "latin1"),
    (16, "hebrew"),
    (18, "tis620"),
    (19, "euckr"),
    (20, "latin7"),
    (21, "latin2"),
    (22, "koi8u"),
    (23, "cp1251"),
    (24, "gb2312"),
    (25, "greek"),
    (26, "cp1250"),
    (27, "latin2"),
    (28, "gbk"),
    (29, "cp1257"),
    (30, "latin5"),
    (31, "latin1"),
    (32, "armscii8"),
    (33, "utf8mb3"),
    (34, "cp1250"),
    (35, "ucs2"),
    (36, "cp866"),
    (37, "keybcs2"),
    (38, "macce"),
    (39, "macroman"),
    (40, "cp852"),
    (41, "latin7"),
    (42, "latin7"),
    (43, "macce"),
    (44, "cp1250"),
    (45, "utf8mb4"),
    (46, "utf8mb4"),
    (47, "latin1"),
    (48, "latin1"),
    (49, "latin1"),
    (50, "cp1251"),
    (51, "cp1251"),
    (52, "cp1251"),
    (53, "macroman"),
    (54, "utf16"),
    (55, "utf16"),
    (56, "utf16le"),
    (57, "cp1256"),
    (58, "cp1257"),
    (59, "cp1257"),
    (60, "utf32"),
    (61, "utf32"),
    (62, "utf16le"),
    (63, "binary"),
    (64, "armscii8"),
    (65, "ascii"),
    (66, "cp1250"),
    (67, "cp1256"),
    (68, "cp866"),
    (69, "dec8"),
    (70, "greek"),
    (71, "hebrew"),
    (72, "hp8"),
    (73, "keybcs2"),
    (74, "koi8r"),
    (75, "koi8u"),
    (76, "utf8mb3"),
    (77, "latin2"),
    (78, "latin5"),
    (79, "latin7"),
    (80, "cp850"),
    (81, "cp852"),
    (82, "swe7"),
    (83, "utf8mb3"),
    (84, "big5"),
    (85, "euckr"),
    (86, "gb2312"),
    (87, "gbk"),
    (88, "sjis"),
    (89, "tis620"),
    (90, "ucs2"),
    (91, "ujis"),
    (92, "geostd8"),
    (93, "geostd8"),
    (94, "latin1"),
    (95, "cp932"),
    (96, "cp932"),
    (97, "eucjpms"),
    (98, "eucjpms"),
    (99, "cp1250"),
    (101, "utf16"),
    (102, "utf16"),
    (103, "utf16"),
    (104, "utf16"),
    (105, "utf16"),
    (106, "utf16"),
    (107, "utf16"),
    (108, "utf16"),
    (109, "utf16"),
    (110, "utf16"),
    (111, "utf16"),
    (112, "utf16"),
    (113, "utf16"),
    (114, "utf16"),
    (115, "utf16"),
    (116, "utf16"),
    (117, "utf16"),
    (118, "utf16"),
    (119, "utf16"),
    (120, "utf16"),
    (121, "utf16"),
    (122, "utf16"),
    (123, "utf16"),
    (124, "utf16"),
    (128, "ucs2"),
    (129, "ucs2"),
    (130, "ucs2"),
    (131, "ucs2"),
    (132, "ucs2"),
    (133, "ucs2"),
    (134, "ucs2"),
    (135, "ucs2"),
    (136, "ucs2"),
    (137, "ucs2"),
    (138, "ucs2"),
    (139, "ucs2"),
    (140, "ucs2"),
    (141, "ucs2"),
    (142, "ucs2"),
    (143, "ucs2"),
    (144, "ucs2"),
    (145, "ucs2"),
    (146, "ucs2"),
    (147, "ucs2"),
    (148, "ucs2"),
    (149, "ucs2"),
    (150, "ucs2"),
    (151, "ucs2"),
    (159, "ucs2"),
    (160, "utf32"),
    (161, "utf32"),
    (162, "utf32"),
    (163, "utf32"),
    (164, "utf32"),
    (165, "utf32"),
    (166, "utf32"),
    (167, "utf32"),
    (168, "utf32"),
    (169, "utf32"),
    (170, "utf32"),
    (171, "utf32"),
    (172, "utf32"),
    (173, "utf32"),
    (174, "utf32"),
    (175, "utf32"),
    (176, "utf32"),
    (177, "utf32"),
    (178, "utf32"),
    (179, "utf32"),
    (180, "utf32"),
    (181, "utf32"),
    (182, "utf32"),
    (183, "utf32"),
    (192, "utf8mb3"),
    (193, "utf8mb3"),
    (194, "utf8mb3"),
    (195, "utf8mb3"),
    (196, "utf8mb3"),
    (197, "utf8mb3"),
    (198, "utf8mb3"),
    (199, "utf8mb3"),
    (200, "utf8mb3"),
    (201, "utf8mb3"),
    (202, "utf8mb3"),
    (203, "utf8mb3"),
    (204, "utf8mb3"),
    (205, "utf8mb3"),
    (206, "utf8mb3"),
    (207, "utf8mb3"),
    (208, "utf8mb3"),
    (209, "utf8mb3"),
    (210, "utf8mb3"),
    (211, "utf8mb3"),
    (212, "utf8mb3"),
    (213, "utf8mb3"),
    (214, "utf8mb3"),
    (215, "utf8mb3"),
    (223, "utf8mb3"),
    (224, "utf8mb4"),
    (225, "utf8mb4"),
    (226, "utf8mb4"),
    (227, "utf8mb4"),
    (228, "utf8mb4"),
    (229, "utf8mb4"),
    (230, "utf8mb4"),
    (231, "utf8mb4"),
    (232, "utf8mb4"),
    (233, "utf8mb4"),
    (234, "utf8mb4"),
    (235, "utf8mb4"),
    (236, "utf8mb4"),
    (237, "utf8mb4"),
    (238, "utf8mb4"),
    (239, "utf8mb4"),
    (240, "utf8mb4"),
    (241, "utf8mb4"),
    (242, "utf8mb4"),
    (243, "utf8mb4"),
    (244, "utf8mb4"),
    (245, "utf8mb4"),
    (246, "utf8mb4"),
    (247, "utf8mb4"),
    (248, "gb18030"),
    (249, "gb18030"),
    (250, "gb18030"),
    (255, "utf8mb4"),
];

/// Default collation of each character set, except `utf8mb4` (see
/// [`default_collation`]).
const DEFAULT_COLLATIONS: &[(&str, u8)] = &[
    ("armscii8", 32),
    ("ascii", 11),
    ("big5", 1),
    ("binary", 63),
    ("cp1250", 26),
    ("cp1251", 51),
    ("cp1256", 57),
    ("cp1257", 59),
    ("cp850", 4),
    ("cp852", 40),
    ("cp866", 36),
    ("cp932", 95),
    ("dec8", 3),
    ("eucjpms", 97),
    ("euckr", 19),
    ("gb18030", 248),
    ("gb2312", 24),
    ("gbk", 28),
    ("geostd8", 92),
    ("greek", 25),
    ("hebrew", 16),
    ("hp8", 6),
    ("keybcs2", 37),
    ("koi8r", 7),
    ("koi8u", 22),
    ("latin1", 8),
    ("latin2", 9),
    ("latin5", 30),
    ("latin7", 41),
    ("macce", 38),
    ("macroman", 39),
    ("sjis", 13),
    ("swe7", 10),
    ("tis620", 18),
    ("ucs2", 35),
    ("ujis", 12),
    ("utf16", 54),
    ("utf16le", 56),
    ("utf32", 60),
    ("utf8mb3", 33),
];
/// `utf8mb4_0900_ai_ci`, MySQL 8's default (MySQL only).
const MYSQL_UTF8MB4_DEFAULT: u8 = 255;
/// `utf8mb4_general_ci`: MariaDB's default with an id that fits the
/// handshake; 11.4+ maps it to its own default (`utf8mb4_uca1400_ai_ci`).
const MARIADB_UTF8MB4_DEFAULT: u8 = 45;

/// Character set of the collation `id`, as named by the server.
#[must_use]
pub fn charset_of(id: u8) -> Option<&'static str> {
    COLLATION_CHARSETS
        .binary_search_by_key(&id, |&(i, _)| i)
        .ok()
        .and_then(|at| COLLATION_CHARSETS.get(at))
        .map(|&(_, name)| name)
}

/// Collation id to request in the handshake for `charset`: the one the
/// server announced in its greeting when it belongs to `charset` (so the
/// session matches the server's default, which its tables use), otherwise
/// the character set's default. `None` for an unknown character set.
#[must_use]
pub fn handshake_collation(charset: &str, greeting: u8, mariadb: bool) -> Option<u8> {
    if charset_of(greeting) == Some(charset) {
        return Some(greeting);
    }
    if charset == "utf8mb4" {
        return Some(if mariadb {
            MARIADB_UTF8MB4_DEFAULT
        } else {
            MYSQL_UTF8MB4_DEFAULT
        });
    }
    DEFAULT_COLLATIONS
        .iter()
        .find(|&&(name, _)| name == charset)
        .map(|&(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::COLLATION_CHARSETS;
    use super::DEFAULT_COLLATIONS;
    use super::charset_of;
    use super::handshake_collation;

    #[test]
    fn tables_are_sorted_and_consistent() {
        assert!(COLLATION_CHARSETS.windows(2).all(|w| w[0].0 < w[1].0));
        for &(name, id) in DEFAULT_COLLATIONS {
            assert_eq!(charset_of(id), Some(name), "{name}");
        }
    }

    #[test]
    fn known_ids() {
        assert_eq!(charset_of(45), Some("utf8mb4"));
        assert_eq!(charset_of(255), Some("utf8mb4"));
        assert_eq!(charset_of(8), Some("latin1"));
        assert_eq!(charset_of(63), Some("binary"));
        assert_eq!(charset_of(2), Some("latin2"));
        assert_eq!(charset_of(0), None);
    }

    #[test]
    fn handshake_collation_prefers_the_greeting() {
        // MySQL 8 greets with utf8mb4_0900_ai_ci, MariaDB with general_ci.
        assert_eq!(handshake_collation("utf8mb4", 255, false), Some(255));
        assert_eq!(handshake_collation("utf8mb4", 45, true), Some(45));
        // Another collation of the same character set is kept as is.
        assert_eq!(handshake_collation("utf8mb4", 224, false), Some(224));
        // Server defaulting to latin1.
        assert_eq!(handshake_collation("utf8mb4", 8, false), Some(255));
        assert_eq!(handshake_collation("utf8mb4", 8, true), Some(45));
        assert_eq!(handshake_collation("latin1", 255, false), Some(8));
        assert_eq!(handshake_collation("cp1251", 45, true), Some(51));
        assert_eq!(handshake_collation("gbk", 255, false), Some(28));
        assert_eq!(handshake_collation("klingon", 255, false), None);
    }
}
