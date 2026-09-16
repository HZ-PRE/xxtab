"""Bounded read-only HTTP timing probe. Never records URLs, cookies or bodies.

Usage: python tools/measure-http.py URL OUTPUT_JSON [COUNT]
Only follows redirects to the same host/port/scheme. No environment proxy.
"""
import concurrent.futures
import html
import http.client
import http.cookiejar
import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def main():
    url, output = sys.argv[1:3]
    count = int(sys.argv[3]) if len(sys.argv) > 3 else 8
    if not 1 <= count <= 20:
        raise ValueError("count must be 1..20")
    target = urllib.parse.urlsplit(url)
    if target.scheme not in ("http", "https") or target.username or target.password:
        raise ValueError("expected HTTP(S) URL without credentials")

    def origin(value):
        parsed = urllib.parse.urlsplit(value)
        return parsed.scheme, parsed.hostname, parsed.port or (443 if parsed.scheme == "https" else 80)

    class SameOriginRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            if origin(newurl) != origin(url):
                raise ValueError("redirect outside test origin")
            return super().redirect_request(req, fp, code, msg, headers, newurl)

    results = {"requests": [], "page": {}, "assets": []}
    for index in range(count):
        cls = http.client.HTTPSConnection if target.scheme == "https" else http.client.HTTPConnection
        connection = cls(target.hostname, target.port, timeout=5)
        start = time.perf_counter()
        row = {"sample": index + 1}
        try:
            connection.connect()
            row["tcp_ms"] = round((time.perf_counter() - start) * 1000, 1)
            path = urllib.parse.urlunsplit(("", "", target.path or "/", target.query, ""))
            connection.request("GET", path, headers={"Connection": "close", "Accept-Encoding": "identity"})
            response = connection.getresponse()
            row.update(status=response.status, ttfb_ms=round((time.perf_counter() - start) * 1000, 1))
            body = response.read(2 * 1024 * 1024)
            row.update(bytes=len(body), total_ms=round((time.perf_counter() - start) * 1000, 1))
        except Exception as error:
            row["error"] = type(error).__name__
        finally:
            connection.close()
        results["requests"].append(row)
        print(json.dumps(row), flush=True)
        time.sleep(0.3)

    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}),
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()),
        SameOriginRedirect(),
    )
    start = time.perf_counter()
    try:
        try:
            response = opener.open(url, timeout=5)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            body = response.read(2 * 1024 * 1024)
            results["page"] = dict(status=response.status, bytes=len(body), total_ms=round((time.perf_counter() - start) * 1000, 1))
            assets = re.findall(r'''(?:src|href)=["']([^"']+)''', body.decode("utf-8", "replace"))
            assets = list(dict.fromkeys(urllib.parse.urljoin(response.url, html.unescape(item)) for item in assets))
            assets = [item for item in assets if origin(item) == origin(url) and urllib.parse.urlsplit(item).path.endswith((".js", ".css"))][:8]

        def fetch_asset(item):
            start = time.perf_counter()
            try:
                # Static assets only, no authenticated actions or form submissions.
                with opener.open(item, timeout=5) as asset:
                    ttfb = round((time.perf_counter() - start) * 1000, 1)
                    size = len(asset.read(2 * 1024 * 1024))
                    return dict(status=asset.status, bytes=size, limit_reached=size == 2 * 1024 * 1024,
                                content_encoding=asset.headers.get("Content-Encoding", "identity"),
                                ttfb_ms=ttfb, total_ms=round((time.perf_counter() - start) * 1000, 1))
            except urllib.error.HTTPError as error:
                return {"error": "HTTPError", "status": error.code}
            except Exception as error:
                return {"error": type(error).__name__}

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            results["assets"] = list(pool.map(fetch_asset, assets))
    except Exception as error:
        results["page"] = {"error": type(error).__name__}
    print(json.dumps({"page": results["page"], "assets": results["assets"]}), flush=True)
    Path(output).write_text(json.dumps(results, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
