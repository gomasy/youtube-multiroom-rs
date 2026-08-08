use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
};
use tower_http::services::ServeDir;

/// A content-hashed name is never reused, so its bytes can be held forever.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// index.html, the manifest, the PWA icons and the locale catalogs keep their
/// names across deploys. ServeDir answers the revalidation with a 304 from its
/// ETag, so a CDN still absorbs the bytes; `public` is what lets it store them.
const REVALIDATE: &str = "public, max-age=0, must-revalidate";

/// The app shell and the message catalogs. Kept out of main.rs's router so the
/// layer below cannot reach /api, and merged in after the auth layer so they
/// stay public (see the comment there).
pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    // Relative to the working directory; the Dockerfile copies both under its WORKDIR.
    router_for("front/dist")
}

/// The build output is the one directory a test needs to substitute, since
/// front/dist only exists once the frontend has been built.
fn router_for<S: Clone + Send + Sync + 'static>(dist: &str) -> Router<S> {
    Router::new()
        .nest_service("/locales", ServeDir::new("front/locales"))
        .fallback_service(ServeDir::new(dist))
        .layer(middleware::from_fn(set_cache_control))
}

async fn set_cache_control(request: Request, next: Next) -> Response {
    let value = cache_control_for(request.uri().path());
    let mut response = next.run(request).await;

    // A 404 under a hashed-looking path must not be pinned in the CDN past the
    // deploy that publishes the file.
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    }

    response
}

fn cache_control_for(path: &str) -> &'static str {
    if is_content_hashed(path) {
        IMMUTABLE
    } else {
        REVALIDATE
    }
}

/// Whether the request path names a content-hashed bundle. Reading the name
/// rather than the extension follows the split the build already makes in
/// front/parcel-plugins/namer.cjs, and leaves anything else on the safe side.
fn is_content_hashed(path: &str) -> bool {
    let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
    let Some((stem, _extension)) = name.rsplit_once('.') else {
        return false;
    };
    let Some((_, hash)) = stem.rsplit_once('.') else {
        return false;
    };
    // Parcel writes eight lowercase hex digits; demanding at least that keeps a
    // merely dotted name (`vendor.min.js`) from being served as immutable.
    hash.len() >= 8 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Uri;
    use tower::ServiceExt;

    /// Send a GET through a real router, ServeDir and all.
    async fn get(router: Router<()>, path: &'static str) -> Response {
        let mut request = Request::new(Body::empty());
        *request.uri_mut() = Uri::from_static(path);
        // The router's error type is Infallible, so Ok is the only pattern.
        let Ok(response) = router.oneshot(request).await;
        response
    }

    fn cache_control(response: &Response) -> Option<&str> {
        response.headers().get(header::CACHE_CONTROL)?.to_str().ok()
    }

    // Names taken from a real `parcel build`.
    #[test]
    fn hashed_bundles_are_immutable() {
        assert_eq!(cache_control_for("/front.ade1b22c.js"), IMMUTABLE);
        assert_eq!(cache_control_for("/front.ef82c314.css"), IMMUTABLE);
        assert_eq!(cache_control_for("/logo.e60bc5d3.svg"), IMMUTABLE);
    }

    #[test]
    fn stable_names_revalidate() {
        // The shell names the hashed bundles, so caching it strands the client
        // on the previous deploy.
        assert_eq!(cache_control_for("/"), REVALIDATE);
        assert_eq!(cache_control_for("/index.html"), REVALIDATE);
        assert_eq!(cache_control_for("/locales/ja.json"), REVALIDATE);
        // Stable on purpose (front/parcel-plugins/namer.cjs); immutable would
        // pin a replaced icon on the device.
        assert_eq!(cache_control_for("/manifest.webmanifest"), REVALIDATE);
        assert_eq!(cache_control_for("/icon-192.png"), REVALIDATE);
        assert_eq!(cache_control_for("/icon-maskable-512.png"), REVALIDATE);
        assert_eq!(cache_control_for("/apple-touch-icon.png"), REVALIDATE);
        assert_eq!(cache_control_for("/favicon.png"), REVALIDATE);
    }

    // None of these is Parcel output: merely dotted, too short, non-hex, upper
    // case, a hashed directory, a bare hash.
    #[test]
    fn only_a_hash_shaped_segment_counts() {
        assert_eq!(cache_control_for("/vendor.min.js"), REVALIDATE);
        assert_eq!(cache_control_for("/app.v2.css"), REVALIDATE);
        assert_eq!(cache_control_for("/front.1234567.js"), REVALIDATE);
        assert_eq!(cache_control_for("/app.zzzzzzzz.js"), REVALIDATE);
        assert_eq!(cache_control_for("/app.4F3A2B1C.js"), REVALIDATE);
        assert_eq!(cache_control_for("/4f3a2b1c/index.html"), REVALIDATE);
        assert_eq!(cache_control_for("/4f3a2b1c.js"), REVALIDATE);
    }

    // The choice above only counts if it survives the layer down to ServeDir.
    #[tokio::test]
    async fn a_served_catalog_carries_the_header() {
        let response = get(router(), "/locales/ja.json").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(cache_control(&response), Some(REVALIDATE));
    }

    #[tokio::test]
    async fn the_fallback_service_is_labelled_too() {
        // Every hashed bundle comes from the fallback, not from a route. Under
        // test is the path the response takes, so the committed catalogs stand
        // in for front/dist, which is a build output that need not exist here.
        let response = get(router_for("front/locales"), "/ja.json").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(cache_control(&response), Some(REVALIDATE));
    }

    #[tokio::test]
    async fn a_miss_is_left_uncacheable() {
        // Hash-shaped, so the naive rule would pin this 404 for a year.
        let response = get(router(), "/index.4f3a2b1c.js").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(cache_control(&response), None);
    }
}
