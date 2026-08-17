// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "KakaoTalkAXHelper",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "kakaotalk-ax-helper", targets: ["KakaoTalkAXHelper"])
    ],
    targets: [
        .executableTarget(name: "KakaoTalkAXHelper")
    ]
)
