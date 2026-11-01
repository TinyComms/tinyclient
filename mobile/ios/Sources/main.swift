import Foundation

// The program entry point. Nothing here is UIKit: the GPUI iOS backend (gpui-ce) runs
// UIApplicationMain itself, with its own app and scene delegates, and it finishes launching the
// window when UIKit connects a scene. Calling it from here, not from a scene delegate, keeps one
// owner for the UIKit run loop: a second UIApplicationMain inside willConnectTo crashed at launch.
tinyclient_ios_main()
