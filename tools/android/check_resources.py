#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
"""Checks the Android resources without the Android build tools.

What it catches (each of these otherwise shows up late, in CI, or at run time on a device):

* every name used as ``R.string.x`` / ``R.color.x`` / ``R.drawable.x`` ... in the Java sources
  exists, and every name used in XML (``@color/x``, ``@drawable/x``, ``@string/x``,
  ``@style/x``, ``@xml/x``, ``@mipmap/x``) exists;
* every string of ``values/`` has a Russian translation and the other way round, with the same
  format placeholders (``%1$s``, ``%2$d``, ``%%``), no unescaped apostrophe or quote, and no
  placeholder-less positional mix;
* every identifier that the engine can report (the Rust enums) has a name in the resources:
  threat titles and explanations, formats, categories, severities, metadata keys, reference
  kinds and classes, embedded kinds, macro languages, sterile-copy actions, preview notices;
* no string is dead (unused and not reachable through the dynamic ``Labels`` lookups);
* ``android:`` attributes of the XML exist in the minimum SDK's ``android.R.attr`` (given
  ``--android-jar``), except the few that are newer on purpose and ignored by older systems.

    check_resources.py [--android-jar PATH] [--repo-root DIR]

Exit status 0 when everything is consistent, 1 otherwise.
"""
import argparse
import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

# android: attributes newer than the minimum SDK (26). Older systems ignore unknown attributes,
# so using them is safe; anything else missing from the minimum SDK is a mistake.
NEWER_ATTRS = {
    "localeConfig": 33,
    "enableOnBackInvokedCallback": 33,
    "dataExtractionRules": 31,
    "windowLightNavigationBar": 27,
    "navigationBarColor": 21,
}

# Name prefixes that Labels.java builds at run time (so those strings are not dead).
DYNAMIC_PREFIXES = (
    "threat_", "format_", "cat_", "sev_", "meta_", "ref_kind_", "ref_class_", "emb_kind_",
    "macro_lang_", "action_", "notice_", "push_",
)

# The only permission the app may request: it shows the local, offline support reminder.
ALLOWED_PERMISSION = "android.permission.POST_NOTIFICATIONS"

problems = []


def fail(msg):
    problems.append(msg)


def snake(camel):
    """Mirror of org.arca.viewer.core.Names.snake."""
    out = []
    for ch in camel:
        if "A" <= ch <= "Z":
            if out:
                out.append("_")
            out.append(ch.lower())
        elif ("a" <= ch <= "z") or ("0" <= ch <= "9"):
            out.append(ch)
    return "".join(out)


# ---- reading the resources ------------------------------------------------------------------

def read_strings(res_dir):
    """Returns {locale: {name: (value, translatable)}} for values/ (en) and values-xx/."""
    result = {}
    for entry in sorted(os.listdir(res_dir)):
        if not entry.startswith("values"):
            continue
        locale = "en" if entry == "values" else entry[len("values-"):]
        if "night" in entry or entry.count("-") > 1:
            continue
        strings = result.setdefault(locale, {})
        for fname in sorted(os.listdir(os.path.join(res_dir, entry))):
            if not fname.endswith(".xml"):
                continue
            path = os.path.join(res_dir, entry, fname)
            try:
                root = ET.parse(path).getroot()
            except ET.ParseError as e:
                fail(f"{path}: not well-formed XML: {e}")
                continue
            for el in root:
                if el.tag != "string":
                    continue
                name = el.get("name")
                if name in strings:
                    fail(f"{path}: duplicate string '{name}' in {entry}/")
                text = "".join(el.itertext())
                strings[name] = (text, el.get("translatable") != "false")
    return result


def resource_names(res_dir):
    """All resource names by type, as defined by the res/ tree."""
    names = {}

    def add(kind, name):
        names.setdefault(kind, set()).add(name)

    for entry in sorted(os.listdir(res_dir)):
        path = os.path.join(res_dir, entry)
        if not os.path.isdir(path):
            continue
        kind = entry.split("-")[0]
        for fname in sorted(os.listdir(path)):
            fpath = os.path.join(path, fname)
            if kind == "values":
                if not fname.endswith(".xml"):
                    continue
                try:
                    root = ET.parse(fpath).getroot()
                except ET.ParseError:
                    continue
                for el in root:
                    n = el.get("name")
                    if not n:
                        continue
                    t = "style" if el.tag == "style" else el.tag
                    if el.tag == "item":
                        t = el.get("type")
                    if t == "style":
                        n = n.replace(".", "_")
                    add(t, n)
            else:
                add(kind, fname.split(".")[0])
    return names


# ---- Rust enums -----------------------------------------------------------------------------

def enum_variants(path, header):
    text = open(path, encoding="utf-8").read()
    m = re.search(header, text)
    if not m:
        fail(f"{path}: enum matching /{header}/ not found")
        return []
    start = text.index("{", m.end() - 1)
    depth = 0
    end = start
    for i in range(start, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                end = i
                break
    body = text[start + 1:end]
    out = []
    for line in body.split("\n"):
        s = line.strip()
        if not s or s.startswith("//") or s.startswith("#"):
            continue
        mm = re.match(r"([A-Z][A-Za-z0-9]*)\b", s)
        if mm:
            out.append(mm.group(1))
    return out


def threat_kinds(path):
    text = open(path, encoding="utf-8").read()
    kinds = re.findall(r"^\s+([A-Z][A-Za-z0-9]*) = \d+ => \(\w+, \w+\)", text, re.M)
    if not kinds:
        fail(f"{path}: no threat kinds found")
    return kinds


# ---- placeholders and escaping --------------------------------------------------------------

PLACEHOLDER = re.compile(r"%(?:(\d+)\$)?([sdf])|%%")


def placeholders(value):
    found = []
    for m in PLACEHOLDER.finditer(value):
        if m.group(0) == "%%":
            continue
        found.append((m.group(1), m.group(2)))
    return found


def check_escaping(locale, name, value):
    # In an Android string resource an apostrophe or a double quote must be escaped, and a
    # leading @ or ? must be escaped too.
    for m in re.finditer(r"(?<!\\)'", value):
        fail(f"values[{locale}]/{name}: unescaped apostrophe (use \\' or the typographic ’)")
        break
    for m in re.finditer(r'(?<!\\)"', value):
        fail(f"values[{locale}]/{name}: unescaped double quote")
        break
    if value.startswith(("@", "?")):
        fail(f"values[{locale}]/{name}: starts with @ or ? (escape it)")
    if re.search(r"%[^%\d$sdf]", value.replace("%%", "")) :
        fail(f"values[{locale}]/{name}: stray % that is not a placeholder (write %%)")
    ph = placeholders(value)
    if len(ph) > 1 and any(p[0] is None for p in ph):
        fail(f"values[{locale}]/{name}: several placeholders must be positional (%1$s, %2$d)")


# ---- Java and XML references ----------------------------------------------------------------

JAVA_REF = re.compile(r"\bR\.(string|color|drawable|mipmap|xml|style|dimen|id|layout|array|plurals)\.([A-Za-z0-9_]+)")
XML_REF = re.compile(r"@(string|color|drawable|mipmap|xml|style|dimen|android:[a-z]+)/([A-Za-z0-9_.]+)")


def java_refs(java_dir):
    refs = {}
    for root, _, files in os.walk(java_dir):
        for f in files:
            if f.endswith(".java"):
                path = os.path.join(root, f)
                text = open(path, encoding="utf-8").read()
                for m in JAVA_REF.finditer(text):
                    refs.setdefault((m.group(1), m.group(2)), []).append(os.path.relpath(path, java_dir))
    return refs


def xml_refs(main_dir):
    refs = {}
    for root, _, files in os.walk(main_dir):
        for f in files:
            if f.endswith(".xml"):
                path = os.path.join(root, f)
                text = open(path, encoding="utf-8").read()
                for m in XML_REF.finditer(text):
                    kind, name = m.group(1), m.group(2)
                    if kind.startswith("android:"):
                        continue
                    refs.setdefault((kind, name.replace(".", "_") if kind == "style" else name), []).append(
                        os.path.relpath(path, main_dir))
    return refs


def android_attrs(main_dir):
    attrs = {}
    for root, _, files in os.walk(main_dir):
        for f in files:
            if f.endswith(".xml"):
                path = os.path.join(root, f)
                text = open(path, encoding="utf-8").read()
                for m in re.finditer(r'\bandroid:([A-Za-z][A-Za-z0-9]*)\s*=', text):
                    attrs.setdefault(m.group(1), set()).add(os.path.relpath(path, main_dir))
                for m in re.finditer(r'<item name="android:([A-Za-z][A-Za-z0-9]*)"', text):
                    attrs.setdefault(m.group(1), set()).add(os.path.relpath(path, main_dir))
    return attrs


def jar_attr_names(jar):
    out = subprocess.run(["javap", "-cp", jar, "-public", "android.R$attr"], capture_output=True, text=True)
    if out.returncode != 0:
        return None
    return set(re.findall(r"public static final int (\w+);", out.stdout))


# ---- main -----------------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--android-jar", help="android.jar of the minimum SDK (for android: attributes)")
    ap.add_argument("--repo-root", default=os.path.join(os.path.dirname(__file__), "..", ".."))
    args = ap.parse_args()
    root = os.path.abspath(args.repo_root)
    main_dir = os.path.join(root, "android", "app", "src", "main")
    res_dir = os.path.join(main_dir, "res")
    java_dir = os.path.join(main_dir, "java")

    strings = read_strings(res_dir)
    en = strings.get("en", {})
    ru = strings.get("ru", {})
    if not en:
        fail("values/ has no strings")

    # 1. translations match
    for name, (value, translatable) in en.items():
        check_escaping("en", name, value)
        if translatable and name not in ru:
            fail(f"values-ru/: missing translation of '{name}'")
        if name in ru and translatable and sorted(placeholders(value)) != sorted(placeholders(ru[name][0])):
            fail(f"'{name}': placeholders differ between en {placeholders(value)} and ru {placeholders(ru[name][0])}")
    for name, (value, _) in ru.items():
        check_escaping("ru", name, value)
        if name not in en:
            fail(f"values-ru/: '{name}' does not exist in values/")
        elif not en[name][1]:
            fail(f"values-ru/: '{name}' is marked translatable=false in values/")
    for name, (value, translatable) in en.items():
        if translatable and not value.strip():
            fail(f"values/{name}: empty")

    # 2. the engine's identifiers all have names
    rs = os.path.join(root, "crates")
    report = os.path.join(rs, "arca-xray", "src", "report.rs")
    expected = {}
    for kind in threat_kinds(report):
        expected[f"threat_{snake(kind)}_title"] = f"ThreatKind::{kind}"
        expected[f"threat_{snake(kind)}_text"] = f"ThreatKind::{kind}"
    groups = [
        ("format_", os.path.join(rs, "arca-xray", "src", "format.rs"), r"pub enum FileFormat \{"),
        ("cat_", report, r"pub enum Category \{"),
        ("sev_", report, r"pub enum Severity \{"),
        ("meta_", report, r"pub enum MetaKey \{"),
        ("ref_kind_", report, r"pub enum RefKind \{"),
        ("ref_class_", report, r"pub enum TargetClass \{"),
        ("emb_kind_", report, r"pub enum EmbeddedKind \{"),
        ("macro_lang_", report, r"pub enum MacroLanguage \{"),
        ("action_", os.path.join(rs, "arca-xray", "src", "sanitize", "mod.rs"), r"pub enum ActionKind \{"),
        ("notice_", os.path.join(rs, "arca-preview", "src", "model.rs"), r"pub enum NoticeKind \{"),
    ]
    for prefix, path, header in groups:
        for variant in enum_variants(path, header):
            expected[prefix + snake(variant)] = os.path.basename(path) + "::" + variant
    for name, origin in sorted(expected.items()):
        if name not in en:
            fail(f"values/: missing '{name}' (for {origin})")
        if name not in ru:
            fail(f"values-ru/: missing '{name}' (for {origin})")

    # 3. references resolve
    defined = resource_names(res_dir)
    used_strings = set()
    for (kind, name), where in sorted(java_refs(java_dir).items()):
        if kind == "string":
            used_strings.add(name)
        # R.id is created by `@+id`; Java may only use ids that XML declares.
        if name not in defined.get(kind, set()):
            fail(f"R.{kind}.{name} used in {where[0]} but not defined in res/")
    for (kind, name), where in sorted(xml_refs(main_dir).items()):
        if kind == "string":
            used_strings.add(name)
        if name not in defined.get(kind, set()):
            fail(f"@{kind}/{name} used in {where[0]} but not defined in res/")
    manifest = open(os.path.join(main_dir, "AndroidManifest.xml"), encoding="utf-8").read()
    manifest = re.sub(r"<!--.*?-->", "", manifest, flags=re.S)  # comments may mention what is absent
    for m in re.finditer(r"@(string|mipmap|style|xml|drawable)/([A-Za-z0-9_.]+)", manifest):
        kind, name = m.group(1), m.group(2).replace(".", "_")
        if kind == "string":
            used_strings.add(name)
        if name not in defined.get(kind, set()):
            fail(f"AndroidManifest.xml refers to @{kind}/{m.group(2)} which is not defined")
    # 4. dead strings
    for name in sorted(en):
        if name in used_strings or name.startswith(DYNAMIC_PREFIXES):
            continue
        if name == "app_name":
            continue
        fail(f"values/{name}: never used")

    # 5. android: attributes exist in the minimum SDK
    if args.android_jar:
        known = jar_attr_names(args.android_jar)
        if known is None:
            fail(f"could not read android.R$attr from {args.android_jar} (is javap installed?)")
        else:
            for attr, where in sorted(android_attrs(main_dir).items()):
                if attr not in known and attr not in NEWER_ATTRS:
                    fail(f"android:{attr} (in {sorted(where)[0]}) does not exist in the minimum SDK and is not in NEWER_ATTRS")

    # 6. manifest sanity: no network, no storage; only local POST_NOTIFICATIONS allowed
    # Parsed, not grepped: every uses-permission* and permission* element counts, whatever the
    # quoting, attribute order or suffix (uses-permission-sdk-23, uses-permission-sdk-m, ...).
    android_name = "{http://schemas.android.com/apk/res/android}name"
    try:
        manifest_root = ET.parse(os.path.join(main_dir, "AndroidManifest.xml")).getroot()
    except ET.ParseError as e:
        manifest_root = None
        fail(f"AndroidManifest.xml: not well-formed XML: {e}")
    if manifest_root is not None:
        for el in manifest_root.iter():
            tag = el.tag if isinstance(el.tag, str) else ""
            if tag.startswith("uses-permission") or tag.startswith("permission"):
                p = el.get(android_name)
                if tag != "uses-permission" or p != ALLOWED_PERMISSION:
                    fail(f"AndroidManifest.xml declares <{tag} {p}>; Arca only allows <uses-permission {ALLOWED_PERMISSION}>")
    if "INTERNET" in manifest:
        fail("AndroidManifest.xml mentions INTERNET")
    if 'android:exported' not in manifest:
        fail("AndroidManifest.xml: activities with intent filters must set android:exported")

    n_expected = len(expected)
    if problems:
        print(f"check_resources: {len(problems)} problem(s)")
        for p in problems[:200]:
            print("  - " + p)
        if len(problems) > 200:
            print(f"  ... and {len(problems) - 200} more")
        return 1
    print(f"check_resources: ok - {len(en)} strings in 2 languages, {n_expected} engine identifiers named, "
          f"{sum(len(v) for v in defined.values())} resources")
    return 0


if __name__ == "__main__":
    sys.exit(main())
