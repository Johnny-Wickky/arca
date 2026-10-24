// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import XCTest
@testable import ArcaKit

final class ArcaKitTests: XCTestCase {

    func testArcaErrorMapping() {
        XCTAssertEqual(ArcaError(code: 0), .ok)
        XCTAssertEqual(ArcaError(code: 1), .cancelled)
        XCTAssertEqual(ArcaError(code: 4), .rejected)
        XCTAssertEqual(ArcaError(code: 6), .encrypted)
        XCTAssertEqual(ArcaError(code: 10), .badHandle)
        XCTAssertEqual(ArcaError(code: 14), .wrongPassword)

        XCTAssertEqual(ArcaError(handleOrCode: -10), .badHandle)
        XCTAssertEqual(ArcaError(handleOrCode: 42), .ok)
        XCTAssertNotNil(ArcaError.rejected.errorDescription)
    }

    func testVerdictDecodingSafety() throws {
        let decoder = JSONDecoder()

        let cleanJson = Data("\"Clean\"".utf8)
        XCTAssertEqual(try decoder.decode(Verdict.self, from: cleanJson), .clean)

        let rejectedJson = Data("\"Rejected\"".utf8)
        XCTAssertEqual(try decoder.decode(Verdict.self, from: rejectedJson), .rejected)

        // Security check: unknown strings must never fall back to clean!
        let unknownJson = Data("\"FutureSafeOrWhatever\"".utf8)
        XCTAssertEqual(try decoder.decode(Verdict.self, from: unknownJson), .unverified)
    }

    func testReportDecoding() throws {
        let json = """
        {
            "format": "Docx",
            "family": "Text",
            "verdict": "HiddenTrackers",
            "rejection": "",
            "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "counters": {
                "trackers": 2,
                "active": 0,
                "findings": 3
            },
            "findings": [
                {
                    "kind": "ExternalImage",
                    "cat": "RemoteContent",
                    "sev": "Medium",
                    "loc": "word/_rels/document.xml.rels",
                    "detail": "https://example.com/pixel.png",
                    "blocked": true
                },
                {
                    "kind": "AuthorLeak",
                    "cat": "PrivacyLeak",
                    "sev": "Low",
                    "loc": "docProps/core.xml",
                    "detail": "John Doe",
                    "blocked": false
                }
            ],
            "macros": [],
            "refs": [
                {
                    "target": "https://example.com/pixel.png",
                    "kind": "Image",
                    "class": "Web",
                    "loc": "word/document.xml",
                    "auto": true
                }
            ],
            "meta": [
                {
                    "key": "Author",
                    "name": "Creator",
                    "value": "John Doe",
                    "src": "core.xml"
                }
            ],
            "embedded": [],
            "stats": {
                "parts": 12,
                "xmlEvents": 1420,
                "inflated": 84200,
                "nesting": 2
            }
        }
        """.data(using: .utf8)!

        let report = try JSONDecoder().decode(Report.self, from: json)
        XCTAssertEqual(report.format, "Docx")
        XCTAssertEqual(report.family, "Text")
        XCTAssertEqual(report.verdict, .hiddenTrackers)
        XCTAssertEqual(report.trackers, 2)
        XCTAssertEqual(report.active, 0)
        XCTAssertEqual(report.findings.count, 2)
        XCTAssertEqual(report.topSeverityRank, 2) // Medium = 2
        XCTAssertFalse(report.needsPassword)
        XCTAssertEqual(report.refs.first?.isAuto, true)
        XCTAssertEqual(report.meta.first?.key, "Author")
        XCTAssertEqual(report.inflated, 84200)
    }

    func testPreviewItemsParsing() {
        let jsonArray: [[String: Any]] = [
            [
                "k": "p",
                "s": "Heading1",
                "a": 0,
                "l": 0,
                "m": "1.",
                "r": [
                    ["Section One: Introduction", 1, ""],
                    [" (External link)", 0, "https://example.com"]
                ]
            ],
            [
                "k": "tr",
                "n": 2,
                "first": true,
                "last": false,
                "cells": [
                    [
                        "c": 1,
                        "h": true,
                        "p": [
                            ["s": "Body", "a": 0, "l": 0, "m": "", "r": [["Header Col", 1]]]
                        ]
                    ]
                ]
            ],
            [
                "k": "gh",
                "cols": 3,
                "w": [10, 20, 15],
                "rows": 100,
                "trunc": false
            ],
            [
                "k": "gr",
                "i": 0,
                "h": false,
                "c": [
                    [0, 0, "Alice"],
                    [1, 1, "42"]
                ]
            ],
            [
                "k": "i",
                "alt": "Chart",
                "media": "png",
                "bytes": 2048,
                "ext": false,
                "id": 4
            ],
            [
                "k": "r"
            ]
        ]

        let items = jsonArray.compactMap(PreviewItem.parse)
        XCTAssertEqual(items.count, 6)

        if case .paragraph(let para) = items[0] {
            XCTAssertEqual(para.style, "Heading1")
            XCTAssertEqual(para.marker, "1.")
            XCTAssertEqual(para.runs.count, 2)
            XCTAssertEqual(para.plainText, "Section One: Introduction (External link)")
            XCTAssertTrue(para.runs[0].style.contains(.bold))
            XCTAssertEqual(para.runs[1].href, "https://example.com")
        } else {
            XCTFail("Expected paragraph")
        }

        if case .tableRow(let row) = items[1] {
            XCTAssertEqual(row.columns, 2)
            XCTAssertTrue(row.isFirst)
            XCTAssertEqual(row.cells.count, 1)
            XCTAssertTrue(row.cells[0].isHeader)
        } else {
            XCTFail("Expected table row")
        }

        if case .gridHeader(let gh) = items[2] {
            XCTAssertEqual(gh.cols, 3)
            XCTAssertEqual(gh.widths, [10, 20, 15])
            XCTAssertEqual(gh.totalRows, 100)
        } else {
            XCTFail("Expected grid header")
        }

        if case .gridRow(let gr) = items[3] {
            XCTAssertEqual(gr.index, 0)
            XCTAssertFalse(gr.isHidden)
            XCTAssertEqual(gr.cells.count, 2)
            XCTAssertEqual(gr.cells[0].text, "Alice")
        } else {
            XCTFail("Expected grid row")
        }

        if case .image(let img) = items[4] {
            XCTAssertEqual(img.alt, "Chart")
            XCTAssertEqual(img.byteSize, 2048)
            XCTAssertEqual(img.imageId, 4)
        } else {
            XCTFail("Expected image item")
        }

        if case .horizontalRule = items[5] {
            // Success
        } else {
            XCTFail("Expected horizontal rule")
        }
    }

    func testSanitizedInfoDecoding() throws {
        let json = """
        {
            "format": "Docx",
            "size": 45120,
            "actions": [
                {
                    "kind": "StripMetadata",
                    "target": "docProps/core.xml"
                },
                {
                    "kind": "RemoveMacros",
                    "target": "word/vbaProject.bin"
                }
            ],
            "verification": {
                "verdict": "Clean",
                "sha256": "4b227777d4dd1fc61c6f884f48641d02b4d121d3fd328cb08b5531fcacdabf8a",
                "findings": 0
            }
        }
        """.data(using: .utf8)!

        let sanitized = try JSONDecoder().decode(SanitizedInfo.self, from: json)
        XCTAssertEqual(sanitized.format, "Docx")
        XCTAssertEqual(sanitized.size, 45120)
        XCTAssertEqual(sanitized.actions.count, 2)
        XCTAssertEqual(sanitized.verification.verdict, .clean)
        XCTAssertEqual(sanitized.verification.findings, 0)
    }

    func testPatronAboutDecoding() throws {
        let json = """
        {
            "appName": "Arca",
            "fingerprint": "a1b2c3d4e5f600112233445566778899aabbccddeeff00112233445566778899",
            "screen": {
                "lang": "ru",
                "cards": [
                    {
                        "role": "Upstream",
                        "title": "Создатели оригинального ядра Arca",
                        "project": "Arca Document Engine",
                        "people": [
                            { "name": "Pharos", "url": "https://example.com" }
                        ],
                        "supportLabel": "Поддержать авторов",
                        "support": [
                            { "label": "Lava.top", "url": "https://lava.top/arca" }
                        ],
                        "homepage": "https://github.com/Johnny-Wickky/arca",
                        "source": "https://github.com/Johnny-Wickky/arca",
                        "modifications": ""
                    }
                ],
                "legal": "AGPLv3",
                "modified": ""
            }
        }
        """.data(using: .utf8)!

        let about = try JSONDecoder().decode(AboutInfo.self, from: json)
        XCTAssertEqual(about.appName, "Arca")
        XCTAssertEqual(about.screen.cards.count, 1)
        XCTAssertTrue(about.screen.cards[0].isUpstream)
        XCTAssertEqual(about.screen.cards[0].people.first?.name, "Pharos")
    }
}
