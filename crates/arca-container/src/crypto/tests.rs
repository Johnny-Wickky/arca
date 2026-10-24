use super::*;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).filter_map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Messages of every length around the block boundaries, hashed by Python's `hashlib`.
const HASH_VECTORS: &[(usize, &str, &str, &str, &str, &str)] = &[
    (
        0,
        "d41d8cd98f00b204e9800998ecf8427e",
        "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b",
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
    ),
    (
        1,
        "55a54008ad1ba589aa210d2629c1df41",
        "bf8b4530d8d246dd74ac53a13471bba17941dff7",
        "4bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459a",
        "8d2ce87d86f55fcfab770a047b090da23270fa206832dfea7e0c946fff451f819add242374be551b0d6318ed6c7d41d8",
        "7b54b66836c1fbdd13d2441d9e1434dc62ca677fb68f5fe66a464baadecdbd00576f8d6b5ac3bcc80844b7d50b1cc6603444bbe7cfcf8fc0aa1ee3c636d9e339",
    ),
    (
        3,
        "c9aee4810523ef8658121b8d492c6b41",
        "4201de9f98cb0a9b8cf52398be7802b55d45266a",
        "6ab0dba1f4f1dfbb37b4f9eeb092c09fca4900ad32bdcd147d8dde35d6c87c35",
        "de5060203c58b419f136c17666d339a05705575827e3600f0475f6fb3089870aede26c5a0903dea5938bcded4ee9e619",
        "8853167ab14c043919981a6409307ef68b5b726f4b80dcab259daa24b7cabcb8b4bbfd3e2e36ed98465059661f672e6529cdcc673e7eabfa78dd3186745b112a",
    ),
    (
        55,
        "c4f7a596fd044675728399e605766107",
        "04f893eac3e6c3714b48ab7525103f7327b45271",
        "d604dc83c754bb5890ec011f3fa0552324cb1412205b5a5e83fd17cd4afe57ed",
        "23f426b139a9ac2935819929d1c3d2870bf172f92bba1226caf927570eb61a562a1cba0988d4877e1e44f02776bd0c7e",
        "df3185830724d08f9e09b9fdf9d3d10a9e87dd98eb54c43609eaaacdfe51c7d999beeb21d24bd98a65788b11e2e53ce50688b24a04931649040103b3d2dc3f67",
    ),
    (
        56,
        "870790e1df8b8ea4beb3e002d45cf18d",
        "5b05376842a5618186792e2ba9882a47ab8a8f61",
        "0371e3fd4911ce4109ac9aaddc0ebdfe28431802ef70a966cb245da585f2a37e",
        "72125d93f0aba5ba496259c380e6eb91f805dc4aff30397ecba05e2b836320b7ed45a20e29419fa7ea2e59a8c66d3bec",
        "d0f1aed94332efa5639957d97d9a8e322180779a58d64f00c0127f72c6bf7913c7d41194e17313c20dab21e85c0f67dbe80db84099f97ec690e2136484d21a05",
    ),
    (
        63,
        "87d902f847f4e841f5ff6061bd6d6e33",
        "bc8885075e915702462e23b6c0f2c099b9d721ca",
        "de8f546e5d44d4fb35d25af2748ed9b6644d200d56b6b0b6e71b095b44348dc6",
        "4dc3ace4fa0b4033bfc00947bee1cbfa8948ca2bd3435437aa6c43532bc77f8a8ab62ce39e0bb5fb353be564791f41ea",
        "5b77430c315188a0972b7c7f4bb062a59a561e727f12ee034701a50432009076ce223dd8e22bd23e293598b368c42776f33b116d877e4ee123d25d3fee181fec",
    ),
    (
        64,
        "d53d88b9c478672e5fb200324158f02e",
        "6304fccb5005d7307678d2013e0e243c3054db5c",
        "35eb8c80b4af970a05040dd151d5edad6172957c1ded27e521fdcf526178feb3",
        "dc4d309ebec9729d257600f436407661e876293572b91ae383499621e4a53c8b05a3495d15dfa616a0d44e8739ac8ad2",
        "61fe5b07f18f84c99cdc1c47a35b06e5832d7b1bf3849b70574314a6ec7964b139f5cc364df1aedc57a6da4ede88e5e4ffc56e57e07dab51e93704b558d73ce5",
    ),
    (
        65,
        "f86b3cccf8ef12ea82a505632db0e9b4",
        "044772bb5a735b181623d9a5b4d4e55dcda6e0b4",
        "c96988663bcd3779da5b1c0e499742ff48bf6148d5c044326ce3e5845a80e6ab",
        "3f6e78497cdcddd8423136c72c685ba5ad9e80d892249b5093214e2fa4a7df6b80e872a7373dd0642241878533623b8a",
        "d8f076835f32073a6b30a53fa77391ed3a1777a7329f460330180727521a88878c2504589bf9391ce45d5fe2f1599a24397170ba99c679b28c33c30f3de61f7d",
    ),
    (
        111,
        "985f5a5db77bd557502a67b4ffe6bf77",
        "6db1c586c085b702459022039fb7569c681c7f9b",
        "dc842a996e8510ece743d33b46a3201593fe91e90f2140785ca4895c440369c1",
        "5c276926149447619dab6a7258a00e91943cb5cb2152a1718b8cd92745c6aa3a43d503526517b4a45e7622e17c8a3346",
        "a1954c0d7783ec96f5c6c98500f3b1c77e759f19858e57744a8f20f80d16eb6ac08033eb24932e2f17bddcc9eeaff9054dd5c37674aa1334f798fc5fb153581d",
    ),
    (
        112,
        "32dc60559b81610c8e3740cc0fe93349",
        "39304f048c4106860960e7d97335a829864b9db2",
        "9f25c2a5d109560e264ac700a9561d55244fc68e70ead8c55ddbde57f167e61e",
        "8d831ffa42acf250b41ee7124f6ca3e1f0008be32ec3e92b1d834583b9ec72c151ca701e4867f04e4dd7f510a0fdd4f6",
        "8b86de42105c9f404d924fd0d6586c05d41a5819fa94d464719c35f8bacad9765f1d27ec066534dfead6c82fadf2ffb6fb291b5b6330dec050fc7238cce92a1d",
    ),
    (
        119,
        "5a74003039c1d72a88148184ab24559a",
        "cd5b017eba674e96bddd0b78cbbada2dfe731030",
        "7fb3869648ad8420f2e1a20812a61ba45c3db01dc7b391d48c2a8c1d6f41be35",
        "79eb2ec54eef2978cd9fae97c47a6048ebe2ad4e6ecf3f69dcda60f83cb8c35ae494713b1fef8018009e481386077dec",
        "62637cd985654b95ca6f5034aa70b5bedcf8f867b23235df6197670b427829e169d91a0b73f79319a3d3b818fadfd401845114b5ef3f5ce15c2c071abe1521d9",
    ),
    (
        120,
        "f9b7a40763fc0331e10dacd92aa5c3ce",
        "8d376d7d411a376cb262704eb6cdfb956839ac8f",
        "9db2bb35e3e98f502b654e82f0051eebfa45015e36ed8c3a28d44586eff7372b",
        "9a8b4986d466397f4c31474f44563e445968f64f98ed1aa528ff42be91f9f12b175bfa12d9946db75a331bf77a0c0d65",
        "db4fa2fea1f8137eccb6a4d40c3b9946d352a67777d3a8d20360c88f84132adafb7634688a4d9f2510f55fc298458a073e34d23e9d0d8dad757238a7f39438c6",
    ),
    (
        127,
        "0ab050c8e2b460f20997f92fcba5a874",
        "1b79872c0b6075124af20b6a3cff7d2ee37b4171",
        "f2a5ec436d60185c6d1c274da6d60d8e38d7de6b739c0fbd043d0de24db2b7ad",
        "97838d81e6d7ff770bd5b9cf2d87315c757946225145746c707478079e85fec2c9784974506143cd375c182a375d1040",
        "ce36abda2e8f4351e53158bae652968d524210b35ef2896bddc1cd5884409548e38100b4f60640e45a7a0719ab4b9de1e716d222752681fd783bee460422e319",
    ),
    (
        128,
        "921f0558bc42b9779a5f0b62c383f1cb",
        "def0a6760ecba9b705b0eb5b458c81af717db1f9",
        "086e64af3be4bef59202dea267776bbbbe6264ad9ff0b46345743fffc446238e",
        "15cb4f8581b438707342ac321dab81ed5a1fb1786e2849c260c10e207fc6aa7fb42eed1d4d35482716d4e0149865c4cc",
        "75c0103fe76fbddfe44f5ddbfd09beedb619570de00e1b1ee43ef688986e89fcc408b32bd4543c6b08287c88c2b543a81cede0f85b37cd1527f0d74977d28142",
    ),
    (
        129,
        "7cdeee8a019e07346948acf5cdf67226",
        "c906373caba36b1d29b181203a5fd4ed0e0d10e5",
        "e565f4d90d1ed354380468d6d343eb77934d2362da37e18371bfc631691f5cab",
        "d94ad88409e7c90bc80fcc72610f61c400af92fcbc16543d22cbfa91a7cee7aadd9388d64ec61da7c481831290d25df9",
        "84dc1ebf8f8cec5e5f0e0f93ac5751b52588e0b0f650b5b799852f701a76be74a7accecc6d867f0fa8494843210f7d86fb7d10936150700027b0c73a70ef6115",
    ),
    (
        200,
        "f26522b2dd4f17c80b50be0ff557703a",
        "bb67915cbff6a1f9c6fed2b48700128df62285cc",
        "2d64ae24c18406f690d51ff1ac9413d823891bf17062693f70eb953108e79178",
        "6c2ee9a031f19a2c28f669c4b4947d5d6915a35572ac13ba16fce1a1322cee90078a966f50e9532002fcfaa9d9ce0b44",
        "24b7a6ae29ab737513fb6b07cff37fc4bb456b0970929bbe46b3e73080dbb2281772b2a967c7daf8ad0abb8b831b3187256e2fa3e8aeacf8b4f977c52e8537f4",
    ),
    (
        1000,
        "05057e6e220d85a40445833ab84fa26b",
        "7f9bb0e12f465c85f91c9d1d61070d1af6e7182c",
        "e66c6563e3797f84573ef7dfe3a460d5d579967297ef20150d89f0c6e358221e",
        "1d400a983d6249f0365686bcdf107c9ad6817a8cdb245d1a04c25e19f68c20b6bdbc27eddef1f73a66cddca20a878697",
        "4ef789652c0eab8b3094e82315fb3bc2f66c6b0b0d51611ad327a915dc6e15a6cae15e2ce75e72ee77454e2e0a9a01de2606984ac5e1977ac19e2ff06851e6cc",
    ),
];

fn message(n: usize) -> Vec<u8> {
    (0..n).map(|i| u8::try_from((i * 7 + n) % 251).unwrap_or(0)).collect()
}

#[test]
fn hashes_match_python_hashlib() {
    for (n, md5, sha1, sha256, sha384, sha512) in HASH_VECTORS {
        let m = message(*n);
        assert_eq!(hex(&Hash::Md5.digest(&[&m])), *md5, "md5 {n}");
        assert_eq!(hex(&Hash::Sha1.digest(&[&m])), *sha1, "sha1 {n}");
        assert_eq!(hex(&Hash::Sha256.digest(&[&m])), *sha256, "sha256 {n}");
        assert_eq!(hex(&Hash::Sha384.digest(&[&m])), *sha384, "sha384 {n}");
        assert_eq!(hex(&Hash::Sha512.digest(&[&m])), *sha512, "sha512 {n}");
        // In pieces, too.
        let (a, b) = m.split_at(n / 3);
        assert_eq!(hex(&Hash::Sha512.digest(&[a, b])), *sha512, "sha512 in two parts {n}");
        assert_eq!(hex(&Hash::Md5.digest(&[a, b])), *md5, "md5 in two parts {n}");
    }
    assert_eq!(Hash::Sha384.size(), 48);
    assert_eq!(hex(&md5::md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(hex(&sha1::sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
}

const AES_VECTORS: &[(&str, &str, &str, &str)] = &[
    (
        "82b70eee7f1a5039bef07ec2347f066e",
        "d08f5dc7512447e3404300026b6e5455",
        "94a065685d64c4980bb8d4544a8721a9",
        "fd7992ef2e9a09641f90dcafb24ceef8|316f7523888c2ef2c2a7d6f76e9a29ac",
    ),
    (
        "9a01ad219eb59cf6a15ef6f15a1d830b",
        "b7ce09d6bbc004e7175c643c7decb0b5",
        "80ec37bc9712dd2e6aaeb94bae8d2f9fa29c5a284c9ef7521829cf1079b080e9",
        "c83f05ce6b554ce3ce75e92e2f2a784854f1824f0d35fc088a7db10580e0f98f|da23c64a5bd64a89341d89979ae3e4d25d84bd0cf975ca788fd9cd0483a29ed3",
    ),
    (
        "d74a1c10fcab6a4243d33656debe4c1e",
        "d79648e856e8f9a2f58c95f0ce4b39c1",
        "5bffad5c2dfb8bb820b6119cba8ff88796ae5b05f280a68ced93b6b28cb0d1b358e6baab485565b9f49028d557d79a8a",
        "5fc2c8d40d554e147096cc6465d19d90496f1f6bd12422702f0dc3c2d8a16c84e43ad245ecafe3e833084d8bd5e10a6d|543ec21c448f8a8070dc14c597cd3b813e5261cb61ec42af2d471d9cf8dea692912762636be88c8b5ada4e33e3fb400c",
    ),
    (
        "0e6451e15c705c15f173541b4438a25cf76312d4eeb3c224",
        "6879bf00b3cf8ed13abf129a3097ad96",
        "b442d6d1bdef4850c3f465442eb300c3",
        "56a295f9412edd498212776ebbfca353|df242caa8cc61c2c40605f1ced392d23",
    ),
    (
        "37a648a6c0dbdd73fc95f5c2c451859afe80d40aa39dfb92",
        "49f40c3ee37d961445c806f58c7cf212",
        "7dfa894f9296fcf33c08409990ac970dedb3b843120181e9376107dbdaf6c5f3",
        "2e3db3ed2fe0cb201a98e220606d2525f939b5d05a5d6999a78b6bcd1e43a4bc|350b195eb30d4928e24e1502893842bda9786a20287f43ce347701d87375e70a",
    ),
    (
        "c86497ee219b01dd92f19f4954f4fea94ed9182375882b20",
        "0daadb23cff9193f3e70384495e04c5d",
        "5ed352226d1637c2248f1d3ccc4405dd2ea1fafab4bf1c46964d947086207882914478bee8c75b4309ae2b122e3fe87a",
        "99970b9b029278dd518a0b2130faff5ee03edfe595baf4fd1fea4e7a21752d6bf2309dc156083e630e7a00ce7c794984|130953e92c3f150396dd3490cdb631e2e75d084c097fdfffb0d9ccaf9ca95d413499388e4216de708eb0b808b5e6a931",
    ),
    (
        "c7ecf5a5370fc41b4ddb723b2afb6c47c0b57894aab2c5c145b797ddb9126e5c",
        "ca203112105f676414faf6b200daf099",
        "dba5eeec33624f5124bfc5f04d82388e",
        "b49f98ee3eb628c087b4a6c87aa8045a|5f7bbad1c19c21a3f17807b7ecfd3673",
    ),
    (
        "52927810f610b0bca11e0be9f14f3ca695e87a5311660c7628cdba9f5eefb990",
        "22ef537b596a16dc8a03ec1fe7d25617",
        "11b3302479fb2ff11b7c19fecb1e1882d0e49c1a13635bce60772ba0372c5226",
        "814d5933d04f3e480a1d46429271b56817f9d5d230f905d325d394087dafafcb|9c6195ac856f21c7c12eafbf3d1bb5068283f8b99d4974f7f8614bd3d68bbbe2",
    ),
    (
        "6d08e1b7f9d8c043069de5723b479ff72c86cea0434229f17d2bdb7d8d1ffc7f",
        "186692bf3224d7a0c20092430ee24909",
        "18da8936c342a4219c564686fea59211200e0e3e1942b6df840876db40b967a9b706535330895749e4dedb3caaa2e474",
        "51025d37fbc268d600fb9e6e3a5ae6f7aafb428786c15f4a2e0755ae48309669a2468242b197cdadd14c8cf8b85f64e1|783102418f34b94f14f1f640198bf3ffb7b35cb3b24774376832a62ae8238b7767fa5b7510f2105a0a30bbbb0af2d3f0",
    ),
];

#[test]
fn aes_matches_the_cryptography_package() {
    for (key, iv, plain, both) in AES_VECTORS {
        let (cbc, ecb) = both.split_once('|').expect("two parts");
        let aes = aes::Aes::new(&unhex(key)).expect("key");
        let (iv, plain) = (unhex(iv), unhex(plain));
        assert_eq!(hex(&aes.cbc_encrypt(&iv, &plain).expect("cbc")), cbc);
        assert_eq!(aes.cbc_decrypt(&iv, &unhex(cbc)).expect("cbc"), plain);
        assert_eq!(aes.ecb_decrypt(&unhex(ecb)), plain);
    }
    // FIPS 197 appendix C.
    let key: Vec<u8> = (0..16).collect();
    let aes = aes::Aes::new(&key).expect("key");
    let mut block = [0u8; 16];
    for (i, b) in block.iter_mut().enumerate() {
        *b = u8::try_from(i * 0x11).unwrap_or(0);
    }
    aes.encrypt_block(&mut block);
    assert_eq!(hex(&block), "69c4e0d86a7b0430d8cdb78070b4c55a");
    aes.decrypt_block(&mut block);
    assert_eq!(hex(&block), "00112233445566778899aabbccddeeff");
    assert!(aes::Aes::new(&[0; 15]).is_none());
    assert!(aes.cbc_decrypt(&[0; 15], &[0; 16]).is_none());
    assert!(aes.cbc_encrypt(&[0; 16], &[0; 17]).is_none());
}

#[test]
fn padding_is_checked() {
    assert_eq!(aes::unpad_pkcs7(vec![7, 2, 2]), Some(vec![7]));
    assert_eq!(aes::unpad_pkcs7(vec![7, 1]), Some(vec![7]));
    assert_eq!(aes::unpad_pkcs7(vec![7, 3, 2]), None);
    assert_eq!(aes::unpad_pkcs7(vec![7, 0]), None);
    assert_eq!(aes::unpad_pkcs7(vec![17; 17]), None);
    assert_eq!(aes::unpad_pkcs7(vec![]), None);
}

#[test]
fn rc4_matches_published_vectors() {
    // RFC 6229, key 0102030405, first bytes of the keystream; and the classic "Key"/"Plaintext".
    let zeros = [0u8; 16];
    assert_eq!(hex(&rc4::rc4(&[1, 2, 3, 4, 5], &zeros)), "b2396305f03dc027ccc3524a0a1118a8");
    assert_eq!(hex(&rc4::rc4(b"Key", b"Plaintext")), "bbf316e8d940af0ad3");
}

#[test]
fn equality_does_not_stop_early() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"ab"));
}

#[test]
fn cbc_decryption_refuses_a_partial_block() {
    let aes = aes::Aes::new(&[7; 16]).expect("key");
    let sealed = aes.cbc_encrypt(&[0; 16], &[0x41; 32]).expect("cbc");
    assert_eq!(aes.cbc_decrypt(&[0; 16], &sealed).expect("whole blocks"), vec![0x41; 32]);
    // Appended or cut-off bytes used to be dropped silently.
    let mut longer = sealed.clone();
    longer.push(0);
    assert!(aes.cbc_decrypt(&[0; 16], &longer).is_none());
    assert!(aes.cbc_decrypt(&[0; 16], &sealed[..31]).is_none());
}

#[test]
fn key_material_does_not_show_in_debug_output() {
    let key = [0xA7u8; 16];
    let aes = aes::Aes::new(&key).expect("key");
    let shown = format!("{aes:?}");
    // The key schedule starts with the key itself; no byte of it may be printed.
    assert!(!shown.contains("167") && !shown.contains("a7") && !shown.contains("A7"), "{shown}");
    let rc4 = rc4::Rc4::new(&[0x55; 5]);
    assert_eq!(format!("{rc4:?}"), "Rc4 { .. }");
}

#[test]
fn bad_padding_of_every_kind_is_the_same_answer() {
    let cases: [Vec<u8>; 6] = [
        vec![7, 0],                                    // zero
        vec![7, 17],                                   // beyond a block
        vec![7, 3, 2],                                 // wrong byte inside the padding
        vec![2],                                       // longer than the data
        vec![9; 16].into_iter().chain([20]).collect(), // beyond a block, long data
        vec![1, 2, 3, 4, 5, 5, 5, 4],                  // padding byte differs from its run
    ];
    for case in cases {
        assert_eq!(aes::unpad_pkcs7(case.clone()), None, "{case:?}");
    }
    assert_eq!(aes::unpad_pkcs7(vec![1; 15].into_iter().chain([1]).collect()), Some(vec![1; 15]));
    assert_eq!(aes::unpad_pkcs7(vec![16; 16]), Some(Vec::new()));
}
