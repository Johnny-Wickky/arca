// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation

/// Person or organisation listed on a creator card.
public struct PatronPerson: Codable, Sendable, Equatable, Identifiable {
    public var id: String { name }

    public let name: String
    public let url: String?

    enum CodingKeys: String, CodingKey {
        case name, url
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        let rawUrl = try c.decodeIfPresent(String.self, forKey: .url) ?? ""
        url = rawUrl.isEmpty ? nil : rawUrl
    }
}

/// External support or donation link.
public struct PatronLink: Codable, Sendable, Equatable, Identifiable {
    public var id: String { url }

    public let label: String
    public let url: String
}

/// Attribution card for original creators and forks.
public struct PatronCard: Decodable, Sendable, Equatable, Identifiable {
    public var id: String { title }

    /// Whether this card represents the original upstream creators of Arca.
    public let isUpstream: Bool
    public let title: String
    public let project: String
    public let people: [PatronPerson]
    public let supportLabel: String
    public let support: [PatronLink]
    public let homepage: String
    public let source: String
    public let modifications: String

    enum CodingKeys: String, CodingKey {
        case role, title, project, people, supportLabel, support, homepage, source, modifications
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let role = try c.decodeIfPresent(String.self, forKey: .role) ?? ""
        isUpstream = (role == "Upstream")
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        project = try c.decodeIfPresent(String.self, forKey: .project) ?? ""
        people = try c.decodeIfPresent([PatronPerson].self, forKey: .people) ?? []
        supportLabel = try c.decodeIfPresent(String.self, forKey: .supportLabel) ?? ""
        support = try c.decodeIfPresent([PatronLink].self, forKey: .support) ?? []
        homepage = try c.decodeIfPresent(String.self, forKey: .homepage) ?? ""
        source = try c.decodeIfPresent(String.self, forKey: .source) ?? ""
        modifications = try c.decodeIfPresent(String.self, forKey: .modifications) ?? ""
    }
}

/// Patronage screen structure per AGPLv3 section 7(b).
public struct PatronScreen: Decodable, Sendable, Equatable {
    public let lang: String
    public let cards: [PatronCard]
    public let legal: String
    public let modified: String?

    enum CodingKeys: String, CodingKey {
        case lang, cards, legal, modified
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        lang = try c.decodeIfPresent(String.self, forKey: .lang) ?? "en"
        cards = try c.decodeIfPresent([PatronCard].self, forKey: .cards) ?? []
        legal = try c.decodeIfPresent(String.self, forKey: .legal) ?? ""
        let rawMod = try c.decodeIfPresent(String.self, forKey: .modified) ?? ""
        modified = rawMod.isEmpty ? nil : rawMod
    }
}

/// Content of the "About & Support" screen.
public struct AboutInfo: Decodable, Sendable, Equatable {
    public let appName: String
    public let fingerprint: String
    public let screen: PatronScreen
}

/// Content of the Guardian's Passport ("Паспорт Хранителя").
public struct PassportInfo: Decodable, Sendable, Equatable {
    public let title: String
    public let body: String
    public let callToAction: String
    public let documentsOpened: Int64
    public let trackersBlocked: Int64
    public let activeContentNeutralized: Int64
    public let daysInUse: Int64
    public let screen: PatronScreen

    enum CodingKeys: String, CodingKey {
        case title, body, cta, documents, trackers, active, days, screen
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        body = try c.decodeIfPresent(String.self, forKey: .body) ?? ""
        callToAction = try c.decodeIfPresent(String.self, forKey: .cta) ?? ""
        documentsOpened = try c.decodeIfPresent(Int64.self, forKey: .documents) ?? 0
        trackersBlocked = try c.decodeIfPresent(Int64.self, forKey: .trackers) ?? 0
        activeContentNeutralized = try c.decodeIfPresent(Int64.self, forKey: .active) ?? 0
        daysInUse = try c.decodeIfPresent(Int64.self, forKey: .days) ?? 0
        screen = try c.decode(PatronScreen.self, forKey: .screen)
    }
}
