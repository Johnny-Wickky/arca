// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import SwiftUI
import CoreGraphics

/// Fullscreen sandboxed image viewer with EXIF forensics.
public struct ImageViewer: View {
    public let image: DecodedImage
    @EnvironmentObject private var state: AppState
    @Environment(\.dismiss) private var dismiss

    @State private var scale: CGFloat = 1.0
    @State private var lastScale: CGFloat = 1.0
    @State private var offset: CGSize = .zero
    @State private var lastOffset: CGSize = .zero
    @State private var isShowingExif = false

    public init(image: DecodedImage) {
        self.image = image
    }

    public var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            // The Rendered Sandbox Image
            if let cgImage = createCGImage(from: image) {
                Image(decorative: cgImage, scale: 1.0)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .scaleEffect(scale)
                    .offset(offset)
                    .gesture(
                        MagnificationGesture()
                            .onChanged { val in
                                let delta = val / lastScale
                                lastScale = val
                                scale = min(max(scale * delta, 0.8), 8.0)
                                if abs(scale - 1.0) < 0.05 {
                                    ArcaHaptics.scaleSnap()
                                }
                            }
                            .onEnded { _ in
                                lastScale = 1.0
                                if scale < 1.0 {
                                    withAnimation(.arcaSpring) {
                                        scale = 1.0
                                        offset = .zero
                                    }
                                }
                            }
                    )
                    .simultaneousGesture(
                        DragGesture()
                            .onChanged { val in
                                offset = CGSize(
                                    width: lastOffset.width + val.translation.width,
                                    height: lastOffset.height + val.translation.height
                                )
                            }
                            .onEnded { _ in
                                lastOffset = offset
                            }
                    )
                    .onTapGesture(count: 2) {
                        withAnimation(.arcaSpring) {
                            if scale > 1.0 {
                                scale = 1.0
                                offset = .zero
                                lastOffset = .zero
                            } else {
                                scale = 2.5
                            }
                            ArcaHaptics.scaleSnap()
                        }
                    }
            } else {
                Text("Ошибка визуализации растра")
                    .foregroundColor(.white)
            }

            // Top Bar
            VStack {
                HStack {
                    Button {
                        dismiss()
                    } label: {
                        ArcaGlyph(.close, size: 20, color: .white)
                            .padding(10)
                            .background(Color.white.opacity(0.2))
                            .clipShape(Circle())
                    }

                    Spacer()

                    if image.facts.exif != nil {
                        Button {
                            isShowingExif.toggle()
                            ArcaHaptics.selection()
                        } label: {
                            HStack(spacing: 6) {
                                ArcaGlyph(.metadata, size: 16, color: .white)
                                Text("EXIF")
                                    .font(.system(size: 13, weight: .bold))
                                    .foregroundColor(.white)
                            }
                            .padding(.horizontal, 12)
                            .padding(.vertical, 6)
                            .background(Color.white.opacity(0.2))
                            .clipShape(Capsule())
                        }
                    }
                }
                .padding(.horizontal, 20)
                .padding(.top, 16)

                Spacer()

                // Resolution & Format Info
                HStack(spacing: 8) {
                    Text("\(image.facts.format) • \(image.facts.width) × \(image.facts.height)")
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                        .foregroundColor(.white.opacity(0.8))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(Color.black.opacity(0.6))
                .clipShape(Capsule())
                .padding(.bottom, 24)
            }
        }
        .sheet(isPresented: $isShowingExif) {
            exifSheet
        }
    }

    // MARK: - EXIF Forensics Sheet

    @ViewBuilder
    private var exifSheet: some View {
        if let exif = image.facts.exif {
            NavigationView {
                ZStack {
                    state.theme.background.ignoresSafeArea()

                    List {
                        Section("КРИМИНАЛИСТИЧЕСКИЕ ДАННЫЕ КАМЕРЫ") {
                            if !exif.make.isEmpty { row(title: "Производитель", value: exif.make) }
                            if !exif.model.isEmpty { row(title: "Модель", value: exif.model) }
                            if !exif.software.isEmpty { row(title: "ПО", value: exif.software) }
                            if !exif.datetime.isEmpty { row(title: "Дата и время", value: exif.datetime) }
                            if !exif.artist.isEmpty { row(title: "Автор", value: exif.artist) }
                            if !exif.owner.isEmpty { row(title: "Владелец", value: exif.owner) }
                            if let pos = exif.position { row(title: "GPS Координаты", value: pos) }
                        }

                        Section("МЕТКИ БЕЗОПАСНОСТИ") {
                            boolRow(title: "Серийный номер", active: exif.hasSerial)
                            boolRow(title: "GPS-геолокация", active: exif.hasGps)
                            boolRow(title: "Миниатюра (Thumbnail)", active: exif.hasThumbnail)
                            boolRow(title: "MakerNote", active: exif.hasMakerNote)
                        }
                    }
                    .scrollContentBackground(.hidden)
                }
                .navigationTitle("EXIF Метаданные")
                .navigationBarTitleDisplayMode(.inline)
            }
        }
    }

    private func row(title: String, value: String) -> some View {
        HStack {
            Text(title).foregroundColor(state.theme.textSecondary)
            Spacer()
            Text(value).foregroundColor(state.theme.textPrimary).fontWeight(.semibold)
        }
    }

    private func boolRow(title: String, active: Bool) -> some View {
        HStack {
            Text(title).foregroundColor(state.theme.textSecondary)
            Spacer()
            Text(active ? "Присутствует" : "Нет")
                .foregroundColor(active ? ArcaTheme.trackerAmber : ArcaTheme.cleanGreen)
                .fontWeight(.bold)
        }
    }

    // MARK: - CoreGraphics RGBA Reconstruction

    private func createCGImage(from img: DecodedImage) -> CGImage? {
        let width = img.facts.width
        let height = img.facts.height
        guard width > 0, height > 0, img.rgbaPixels.count == width * height * 4 else {
            return nil
        }

        let colorSpace = CGColorSpaceCreateDeviceRGB()
        let bitmapInfo = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)

        guard let provider = CGDataProvider(data: img.rgbaPixels as CFData) else {
            return nil
        }

        return CGImage(
            width: width,
            height: height,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: width * 4,
            space: colorSpace,
            bitmapInfo: bitmapInfo,
            provider: provider,
            decode: nil,
            shouldInterpolate: true,
            intent: .defaultIntent
        )
    }
}
