//! Immutable manifest for Lingua 1.3.0 probability model data.
//!
//! Generated from the crates.io packages' `.cargo_vcs_info.json` and
//! `models/ngrams.fst`. Checksums and lengths are checked against those published
//! crates by the offline test suite. Files are data, never downloaded code.

#[derive(Clone, Copy, Debug)]
pub(super) struct ModelSpec {
    pub code: &'static str,
    pub iso_639_3: &'static str,
    pub byte_len: usize,
    pub sha256: &'static str,
}

pub(super) const MODEL_SOURCE_COMMIT: &str = "77e94d1a2a8e49bed873a7c502bcb51ba803ef0f";
pub(super) const MODEL_SOURCE_HOST: &str = "raw.githubusercontent.com";
pub(super) const MAX_MODEL_BYTES: usize = 16 * 1024 * 1024;

pub(super) static MODELS: &[ModelSpec] = &[
    ModelSpec {
        code: "af",
        iso_639_3: "afr",
        byte_len: 2822385,
        sha256: "96a192ca666dc6b5025e71752548f3166c7a5d4f7d9889b4e74d8a609210ab0f",
    },
    ModelSpec {
        code: "sq",
        iso_639_3: "sqi",
        byte_len: 4136949,
        sha256: "ef2e7e40fcca75ea0a119fdddb277a998e199505d33ab87a79e42ae60e428485",
    },
    ModelSpec {
        code: "ar",
        iso_639_3: "ara",
        byte_len: 7920062,
        sha256: "4f617573c5934bbb5e2a626331717551dfbfa4e547de3f8b0f7a1800aa22addc",
    },
    ModelSpec {
        code: "hy",
        iso_639_3: "hye",
        byte_len: 2345686,
        sha256: "eac387e62d0c555ba22aa24f2565ff3cb9f75fd2df93243a7e3d10803ef750c1",
    },
    ModelSpec {
        code: "az",
        iso_639_3: "aze",
        byte_len: 4859634,
        sha256: "bd1b7733e787dd2d130f0f6ceabe1f95003b049d3a64f387802469a19b97fe07",
    },
    ModelSpec {
        code: "eu",
        iso_639_3: "eus",
        byte_len: 2155365,
        sha256: "35e46e49d4424df23bfca151992d5e29652c0227e0a93e9136d59079c291a7c4",
    },
    ModelSpec {
        code: "be",
        iso_639_3: "bel",
        byte_len: 5178678,
        sha256: "60fd4e1e5995d08ef4b653af6e056d4bd783f6fce6c4565384ba1b80fcc0f77f",
    },
    ModelSpec {
        code: "bn",
        iso_639_3: "ben",
        byte_len: 522450,
        sha256: "cbd8d33707d00b4ab97fda59d72b5e529042e08c75083ab95f9cfa52542ec28f",
    },
    ModelSpec {
        code: "nb",
        iso_639_3: "nob",
        byte_len: 4749431,
        sha256: "140fff16a299aeb789fd402681abdc4d2b5a2e41973b4cfe6ba2bdaa1c96584e",
    },
    ModelSpec {
        code: "bs",
        iso_639_3: "bos",
        byte_len: 3106866,
        sha256: "479e70fd3a2b55b2dce462927ef76f74e54835840ff840dd31cd152d379562ae",
    },
    ModelSpec {
        code: "bg",
        iso_639_3: "bul",
        byte_len: 4801489,
        sha256: "9bcf6085712ecf5a254066112c82b2072606f79e8bc09429dc69d4b3fa37a482",
    },
    ModelSpec {
        code: "ca",
        iso_639_3: "cat",
        byte_len: 3600298,
        sha256: "35b157246bbdf7c86abc87457baa56a58a09b9cca8ff9990811aad900c6377cf",
    },
    ModelSpec {
        code: "zh",
        iso_639_3: "zho",
        byte_len: 79185,
        sha256: "7903e64cce8eae7552d48d6bed3b3902d43f57762aed8f669ea821202d6f35bf",
    },
    ModelSpec {
        code: "hr",
        iso_639_3: "hrv",
        byte_len: 5580152,
        sha256: "b9eda5bfd018de7321e8dc97c38b87d366308c3286dee27202bed2cfb3584dbe",
    },
    ModelSpec {
        code: "cs",
        iso_639_3: "ces",
        byte_len: 8540743,
        sha256: "d4a8db420f970b84cab85282be4239b59cdd1a525dbce540fece3c445b63d106",
    },
    ModelSpec {
        code: "da",
        iso_639_3: "dan",
        byte_len: 5310106,
        sha256: "246d578bae6800e576e0cf7f01b8f68f79de157a3f95ce1c9530fd1482e64536",
    },
    ModelSpec {
        code: "nl",
        iso_639_3: "nld",
        byte_len: 5266563,
        sha256: "97ca76be752d26d91daa9330ccfa9c34f16610a74c3f6b264e265d7de5fcce16",
    },
    ModelSpec {
        code: "en",
        iso_639_3: "eng",
        byte_len: 4430546,
        sha256: "d726944d76b2ada98a2fcc495d10f2d5fecaf98edcfc9d2291795832782a03f1",
    },
    ModelSpec {
        code: "eo",
        iso_639_3: "epo",
        byte_len: 4919787,
        sha256: "9f82bb21b9a627f75cb1979db2787c9aad46abfd9cc2e7aafd67ad98a78a7368",
    },
    ModelSpec {
        code: "et",
        iso_639_3: "est",
        byte_len: 5278618,
        sha256: "42b86457ea695db63c710a0a12152c6da77881d73f64ae2f6c597dbd34f4ff0d",
    },
    ModelSpec {
        code: "fi",
        iso_639_3: "fin",
        byte_len: 5028140,
        sha256: "499f55b1b3edfa93f0a0fd80ae47bca156440c66682eee8f290eca33b6c83d20",
    },
    ModelSpec {
        code: "fr",
        iso_639_3: "fra",
        byte_len: 4250295,
        sha256: "d3aa6ad96a63e621cb82976f874631aa290dbd6d182a73da2811e8bba6b4fea2",
    },
    ModelSpec {
        code: "lg",
        iso_639_3: "lug",
        byte_len: 1843433,
        sha256: "0ca9839892257a064c56d715c58ca79a503d21a3a0d157b5ce6c1522f0065a95",
    },
    ModelSpec {
        code: "ka",
        iso_639_3: "kat",
        byte_len: 3975237,
        sha256: "2c1b60f9240e01eb203818bbd629f152b42abd51726b6b69e04825cd47380297",
    },
    ModelSpec {
        code: "de",
        iso_639_3: "deu",
        byte_len: 5575602,
        sha256: "cc4c1201d64ef568f0829f481592191861a652b46ecf0270503a92eca0c71fd1",
    },
    ModelSpec {
        code: "el",
        iso_639_3: "ell",
        byte_len: 6096671,
        sha256: "f133978b967f459bff24023b344fe931275096a3dbdc58126e45ae5a3a80cb6c",
    },
    ModelSpec {
        code: "gu",
        iso_639_3: "guj",
        byte_len: 335612,
        sha256: "bc02bcf43e9a441b8c686cc7e10f9c5500217dcfa58549c31a381b5948e41a8a",
    },
    ModelSpec {
        code: "he",
        iso_639_3: "heb",
        byte_len: 5127074,
        sha256: "fcfd1a924898e1eb0875d65190cc338a2d823ee3fd65bab4e80b8b4aabeb3573",
    },
    ModelSpec {
        code: "hi",
        iso_639_3: "hin",
        byte_len: 435030,
        sha256: "6f3b6ec7dbefa282838aede1359177c9402a1baeeadceba6ea05d45bb4a3fb76",
    },
    ModelSpec {
        code: "hu",
        iso_639_3: "hun",
        byte_len: 9804682,
        sha256: "2d0c9abe0f00f27b5924a09075352bd5b0ebb5524540191d1f5add2df427799d",
    },
    ModelSpec {
        code: "is",
        iso_639_3: "isl",
        byte_len: 5571121,
        sha256: "7b32b35839d84f74bf750e7b142370adf34ca3eeabc473b61ebd161e5dd0f938",
    },
    ModelSpec {
        code: "id",
        iso_639_3: "ind",
        byte_len: 3409356,
        sha256: "1a8d3fcb52141b21ca1c140ea1e7a6f0346b976653ed73d6cc0220171cfc6a07",
    },
    ModelSpec {
        code: "ga",
        iso_639_3: "gle",
        byte_len: 2436932,
        sha256: "915a4a12260f71f430dc585c76964cb87cbe391832e587057a476cd997cde970",
    },
    ModelSpec {
        code: "it",
        iso_639_3: "ita",
        byte_len: 3671542,
        sha256: "cd77c8288701c8092b7436063fca7b685a3bb0d5063e1b083be36bc75efd4d14",
    },
    ModelSpec {
        code: "ja",
        iso_639_3: "jpn",
        byte_len: 62826,
        sha256: "2c3352cebd69ce13ace26921220e524628dee4df23539b23cafdf5d50eeadbca",
    },
    ModelSpec {
        code: "kk",
        iso_639_3: "kaz",
        byte_len: 4838667,
        sha256: "7d67366a5730f2ced87a29a0a6c2ef804588c4b3410d57c2aa8f55776b515e45",
    },
    ModelSpec {
        code: "ko",
        iso_639_3: "kor",
        byte_len: 23000,
        sha256: "45b864f516028ea1f2344ab2f40e6e2b30dd2002e63f3286f3ff4004e8048ebe",
    },
    ModelSpec {
        code: "la",
        iso_639_3: "lat",
        byte_len: 2678330,
        sha256: "d64ccddc7c3edeca14fe0afa96db529d110f676a5730adccccf339e848772788",
    },
    ModelSpec {
        code: "lv",
        iso_639_3: "lav",
        byte_len: 6123019,
        sha256: "28b91f39a09f3b8885047ed5587c1b71f5c403d2ae1ff94282151206a7b67636",
    },
    ModelSpec {
        code: "lt",
        iso_639_3: "lit",
        byte_len: 6210594,
        sha256: "46816b5e449da2a1bd9a61edeeffa482f916b5c4dcd45009fa4931153d167dd6",
    },
    ModelSpec {
        code: "mk",
        iso_639_3: "mkd",
        byte_len: 2702143,
        sha256: "302287a5d532aabb4e32adb38cf58e6852b144c314fbf0486d1f83e08a6d2b0d",
    },
    ModelSpec {
        code: "ms",
        iso_639_3: "msa",
        byte_len: 2030308,
        sha256: "3bc2146b1194eeeaa822756f41d225a0eca10a679468ba04c438d5e4b8160c6a",
    },
    ModelSpec {
        code: "mi",
        iso_639_3: "mri",
        byte_len: 996075,
        sha256: "0ab6b2b6ef1dddb85af69c19a1a35f5ef4011414b41fe1345ae5621fb3e642ff",
    },
    ModelSpec {
        code: "mr",
        iso_639_3: "mar",
        byte_len: 604543,
        sha256: "cfccac6b43fcfe10c6d7b3ab9f4028bbfef2376bb33c265d66037f238a66661d",
    },
    ModelSpec {
        code: "mn",
        iso_639_3: "mon",
        byte_len: 2878730,
        sha256: "ceaae7475d754e2a4c0787a707f5cb2bd09705fea68f230943b8177011c0eb8b",
    },
    ModelSpec {
        code: "nn",
        iso_639_3: "nno",
        byte_len: 3824711,
        sha256: "b7487010bb38811de0564d58706ad2f6da6940db02a28cc9afe18c5a60e3df7b",
    },
    ModelSpec {
        code: "fa",
        iso_639_3: "fas",
        byte_len: 5081552,
        sha256: "395a0f0e475c39e1750bcfdd0fd9c51c049d7610507cb1ab42e5d76fd1a7cd2d",
    },
    ModelSpec {
        code: "pl",
        iso_639_3: "pol",
        byte_len: 5998482,
        sha256: "05dfaa4487e54ba5e7dfa4d3189b032e0681abd2f2ea8d02e80fa7777bc30bb3",
    },
    ModelSpec {
        code: "pt",
        iso_639_3: "por",
        byte_len: 3817500,
        sha256: "755c1aa54ef0badbcfe36acf04f11dbfdb98b7a91620f87ad9ee029a6fed19ba",
    },
    ModelSpec {
        code: "pa",
        iso_639_3: "pan",
        byte_len: 165684,
        sha256: "ec98d39a846b2abbe5eda18080e47d02bc579d4d19d29998e11807c0fb9f696d",
    },
    ModelSpec {
        code: "ro",
        iso_639_3: "ron",
        byte_len: 4998912,
        sha256: "7b56f155395e452064b3463c2b16e28632e71feb43f97fb82ee75aaec2f105f2",
    },
    ModelSpec {
        code: "ru",
        iso_639_3: "rus",
        byte_len: 5135760,
        sha256: "804f4989250b2136b33d771af77171948bfb87cdae9ca21bed83c7fadf09fb51",
    },
    ModelSpec {
        code: "sr",
        iso_639_3: "srp",
        byte_len: 5537843,
        sha256: "50ef373a4d045d9261734afd3ae89ba63349d6243c2605fe87cbc70abaae63a7",
    },
    ModelSpec {
        code: "sn",
        iso_639_3: "sna",
        byte_len: 1035017,
        sha256: "7822e140f71300cee3d074d7e0433958662fa1d2f66d542efc92c167c887aecf",
    },
    ModelSpec {
        code: "sk",
        iso_639_3: "slk",
        byte_len: 7376814,
        sha256: "9d7c295ec791307454e1448aa2395053d7debe79a7608fea5372bd1fc97386e4",
    },
    ModelSpec {
        code: "sl",
        iso_639_3: "slv",
        byte_len: 5230861,
        sha256: "ff401e18098de593797dcbfd0be98d8d6b09648bfc75ffb45a17e3057bd82036",
    },
    ModelSpec {
        code: "so",
        iso_639_3: "som",
        byte_len: 1585233,
        sha256: "612e980dc08bc8a1f63fb710e653b207950086b898a85bc08af294de86c19e11",
    },
    ModelSpec {
        code: "st",
        iso_639_3: "sot",
        byte_len: 501733,
        sha256: "6fceb35972883d4f8ef6a48f1b2df53db03299e08254891b9fd056b4196a0abd",
    },
    ModelSpec {
        code: "es",
        iso_639_3: "spa",
        byte_len: 4186205,
        sha256: "40b0ed8d072389fb8a67af9f300951c6da4838a0149ebaafc3f466173e3a395d",
    },
    ModelSpec {
        code: "sw",
        iso_639_3: "swa",
        byte_len: 696181,
        sha256: "5455102f229ec4874dfb99848058b14c98bf0c53ce852311950be340b595a982",
    },
    ModelSpec {
        code: "sv",
        iso_639_3: "swe",
        byte_len: 5764257,
        sha256: "0c5ea86302f8cd21cc9a8157c55c1a94a8bf61ec81a2ec48c79ea5561406e510",
    },
    ModelSpec {
        code: "tl",
        iso_639_3: "tgl",
        byte_len: 1848987,
        sha256: "8fffed22787621d8236944941958878f8a0b73bc718a17ebc2fa10a601598d5a",
    },
    ModelSpec {
        code: "ta",
        iso_639_3: "tam",
        byte_len: 234650,
        sha256: "ab96fe703bbc3c78e04fc779269a5ac6b43018f02885458dd0bb2a71c834b7c1",
    },
    ModelSpec {
        code: "te",
        iso_639_3: "tel",
        byte_len: 255920,
        sha256: "897aeb7fb263b2239616a745041fcb0a88a9b5f35af6ca67788fa7ed59d436c9",
    },
    ModelSpec {
        code: "th",
        iso_639_3: "tha",
        byte_len: 4612908,
        sha256: "c225e1f069b44141162186a65b358d27a4a42378aa152bf56d28427f6c73148a",
    },
    ModelSpec {
        code: "ts",
        iso_639_3: "tso",
        byte_len: 548458,
        sha256: "de8c91cc6c910eda72263cba1857f9c9cc12791c062642979e17ff7fd1e7fe4e",
    },
    ModelSpec {
        code: "tn",
        iso_639_3: "tsn",
        byte_len: 408360,
        sha256: "66eb2844a18fc43ed69c9a67a6f21fced3ef685c12414ff160ab9f404e3c51b2",
    },
    ModelSpec {
        code: "tr",
        iso_639_3: "tur",
        byte_len: 5674583,
        sha256: "92636e919f71ed56ff478c74368d1c982f70c637b04171a4c2d429986387cb1f",
    },
    ModelSpec {
        code: "uk",
        iso_639_3: "ukr",
        byte_len: 5336246,
        sha256: "f9188c7cdc974ae70acb0894b7da691c02c964bce514f477a0deaa6c2ac651d9",
    },
    ModelSpec {
        code: "ur",
        iso_639_3: "urd",
        byte_len: 5242809,
        sha256: "841f8f706d87f3ab8c2b2051d698719d70418b0bed188d432f028cd7bd2595c1",
    },
    ModelSpec {
        code: "vi",
        iso_639_3: "vie",
        byte_len: 2310807,
        sha256: "48d8d95557a3af8566b86aec25c23b4c70833ced3714932325099b62d5354302",
    },
    ModelSpec {
        code: "cy",
        iso_639_3: "cym",
        byte_len: 2420574,
        sha256: "e981fdc552334c7130ebaf291aac4774c5b258158157243b59387596ef0ef29b",
    },
    ModelSpec {
        code: "xh",
        iso_639_3: "xho",
        byte_len: 1279621,
        sha256: "348f690fad5533d99ecd1fdecb34dc97f41b2694942e4341385252b00857f97c",
    },
    ModelSpec {
        code: "yo",
        iso_639_3: "yor",
        byte_len: 983848,
        sha256: "cad17e9fd714a03b418902f489c538b2b9acec90d1d4da1fc815c19ed3a7a227",
    },
    ModelSpec {
        code: "zu",
        iso_639_3: "zul",
        byte_len: 1859204,
        sha256: "46f7c7ec42de11acf3a75757b8b9d641025c7821b9e5516674112c7d9bd0c0f4",
    },
];

pub(super) fn language_for_tag(tag: &str) -> Option<&'static ModelSpec> {
    let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
    let primary = match primary.as_str() {
        "no" => "nb",
        "fil" => "tl",
        "iw" => "he",
        "in" => "id",
        code => code,
    };
    MODELS
        .iter()
        .find(|model| model.code == primary || model.iso_639_3 == primary)
}

pub(super) fn source_path(model: &ModelSpec) -> String {
    format!(
        "/pemistahl/lingua-rs/{MODEL_SOURCE_COMMIT}/language-models/{}/models/ngrams.fst",
        model.code
    )
}
