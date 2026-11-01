import UIKit

// The program entry point. The GPUI iOS backend (gpui-ce) runs UIApplicationMain itself, with its
// own app and scene delegates, and it finishes launching the window when UIKit connects a scene.
// Calling it from here, not from a scene delegate, keeps one owner for the UIKit run loop: a
// second UIApplicationMain inside willConnectTo crashed at launch.
//
// `import UIKit` is not decorative: it makes Swift link UIKit and CoreGraphics, which the Rust
// static library calls into (CGBitmapContextCreate and friends). Without it the link fails.
tinyclient_ios_main()
