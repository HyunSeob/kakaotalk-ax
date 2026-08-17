// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "KakaoTalkAXHelper",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "kakaotalk-ax-helper", targets: ["KakaoTalkAXHelper"])
    ],
    targets: [
        .target(name: "KakaoTalkAXCore"),
        .executableTarget(name: "KakaoTalkAXHelper", dependencies: ["KakaoTalkAXCore"]),
        .testTarget(name: "KakaoTalkAXCoreTests", dependencies: ["KakaoTalkAXCore"])
    ]
)
