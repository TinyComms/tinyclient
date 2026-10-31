import UIKit

/// Hosts the Rust view.
///
/// Today `Tiny-Bridging-Header.h` declares one function, `tinyclient_ios_main()`, which calls
/// `gui::run()` in Rust. UNVERIFIED SEAM: the pinned GPUI revision has no iOS backend, so this
/// has never linked or run. Which layer owns `UIApplicationMain` is exactly the question the
/// gpui-ce / gpui-pre spike answers:
///
/// * if the Rust platform layer runs its own `UIApplicationMain`, move the call into
///   `AppDelegate.application(_:didFinishLaunchingWithOptions:)` and delete the placeholder
///   view controller here;
/// * if the Rust layer wants a host `UIView`, attach it to `host.view` below.
///
/// Nothing else in this file should need to change either way.
final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = scene as? UIWindowScene else { return }

        let window = UIWindow(windowScene: windowScene)
        let host = UIViewController()
        // Matches the desktop window's default background, so a slow first frame is not a
        // white flash.
        host.view.backgroundColor = .black
        window.rootViewController = host
        self.window = window
        window.makeKeyAndVisible()

        // The Rust side. Blocking on the pin's model (it calls `application.run`); it will be
        // the seam that changes when the mobile backend lands.
        tinyclient_ios_main()
    }
}
