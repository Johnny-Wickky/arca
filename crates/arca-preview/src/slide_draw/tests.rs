//! Tests.
use super::*;

#[test]
fn debug_parse() {
    let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/slides.pptx")).unwrap();
    let policy = PreviewPolicy::default();
    let doc = detect::open(&data, &policy).unwrap();
    if let Doc::Opc { mut pkg, format } = doc {
        let main = pkg.main_part().unwrap().unwrap();
        eprintln!("format {format:?} main {main}");
        let bytes = pkg.read(&main).unwrap().unwrap();
        let text = pkg.decode(&main, &bytes).unwrap().into_owned();
        let p = parse(&text, pkg.limits(), &[], &["sldId"]).unwrap();
        eprintln!("root {} kids {:?}", p.root.name, p.root.kids.iter().map(|k| k.name.clone()).collect::<Vec<_>>());
        let lst = p.root.child("sldIdLst");
        eprintln!("{:?}", lst.map(|l| l.kids.clone()));
    }
}
