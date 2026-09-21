//! Opt-in real Chromium boundary regression. CI installs Playwright and runs it
//! explicitly; ordinary Rust tests do not require Node or a browser installation.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ui dependencies and pnpm --dir ui exec playwright install chromium"]
async fn real_browser_origin_boundary() {
    let h = harness().await;
    let reserved = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let dev_addr = reserved.local_addr().unwrap();
    let mut config = nits_client_web::web_config(
        h.endpoint.clone(),
        client_info(),
        author(),
        IdSeed(700),
        KvConfig::Memory,
    );
    config
        .allowed_origins
        .push(format!("http://{dev_addr}").parse().unwrap());
    let bridge = nits_client_web::serve((Ipv4Addr::LOCALHOST, 0).into(), config)
        .await
        .unwrap();
    let action = Action::CreateReview {
        workspace_id: workspace_id(),
        title: "replaced by browser scenario".into(),
        targets: NonEmpty::singleton(ReviewTarget {
            repo_id: repo_id(),
            base: RefSpec::Branch {
                name: "main".into(),
            },
            head: RefSpec::Branch {
                name: "feature-a".into(),
            },
        }),
    };
    drop(reserved);
    let output = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::process::Command::new("node")
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui"))
            .arg("--input-type=module")
            .arg("--eval")
            .arg(BROWSER)
            .env("BRIDGE_URL", format!("http://{}", bridge.addr()))
            .env("DEV_PORT", dev_addr.port().to_string())
            .env("CREATE_REVIEW", serde_json::to_string(&action).unwrap())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "browser failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let observer = Client::connect_unix(
        &h.dir.path().join("nitsd.sock"),
        Identity {
            client_id: ClientId::from_parts(9, 11),
            client: client_info(),
            author: author(),
        },
    )
    .await
    .unwrap();
    let Response::Reviews { reviews } = observer
        .request(Request::ListReviews {
            workspace_id: workspace_id(),
        })
        .await
        .unwrap()
    else {
        panic!("reviews")
    };
    let mut titles: Vec<_> = reviews.into_iter().map(|review| review.title).collect();
    titles.sort();
    assert_eq!(
        titles,
        [
            "authorized direct",
            "authorized proxy",
            "review a",
            "review b"
        ]
    );
    bridge.stop();
    wait_for_sessions(&bridge, 0).await;
}

// The JavaScript is a browser driver fixture; assertions and daemon setup live
// in this Rust integration test. No browser permission/security switches are used.
const BROWSER: &str = r"
import assert from 'node:assert/strict';
import http from 'node:http';
import { chromium } from 'playwright';
import { createServer } from 'vite';

const bridge = process.env.BRIDGE_URL;
const ws = bridge.replace('http:', 'ws:') + '/ws';
const action = JSON.parse(process.env.CREATE_REVIEW);
const attacker = http.createServer((req, res) => {
  res.writeHead(200, { 'Content-Type': 'text/html' });
  res.end('<!doctype html><title>Unrelated site</title>');
});
await new Promise(resolve => attacker.listen(0, '127.0.0.1', resolve));
// Load the real vite.config.mts; only the dynamically allocated bridge and dev
// ports differ from pnpm dev. The production Host/Origin forwarding is unchanged.
const vite = await createServer({ server: {
  host: '127.0.0.1', port: Number(process.env.DEV_PORT), strictPort: true,
  proxy: { '/ws': { target: bridge.replace('http:', 'ws:'), ws: true } },
} });
await vite.listen();
let browser;
try {
  browser = await chromium.launch({ headless: true });
  async function exercise(page, url, title) {
    return page.evaluate(({url, action, title}) => new Promise((resolve, reject) => {
      let opened = false;
      let frames = 0;
      let read = false;
      let dispatched = false;
      const socket = new WebSocket(url);
      const timer = setTimeout(() => { socket.close(); reject(new Error('bridge timed out')); }, 10000);
      const finish = value => { clearTimeout(timer); socket.close(); resolve(value); };
      socket.onopen = () => {
        opened = true;
        socket.send(JSON.stringify({cmd:'attach'}));
      };
      socket.onmessage = event => {
        frames++;
        const patches = JSON.parse(event.data);
        for (const patch of patches) {
          if (patch.type === 'ReviewList' && patch.workspaces.some(w => w.repos.length)) read = true;
          if (!dispatched && patch.type === 'Connection' && patch.connection.type === 'Subscribed') {
            dispatched = true;
            socket.send(JSON.stringify({cmd:'dispatch', action:{...action, title}}));
          }
          if (read && patch.type === 'ReviewList' && patch.reviews.some(r => r.title === title)) {
            finish({opened, frames, read, dispatched});
          }
        }
      };
      socket.onclose = () => finish({opened, frames, read, dispatched});
      socket.onerror = () => {};
    }), {url, action, title});
  }
  const direct = await browser.newPage();
  await direct.goto(bridge);
  await direct.getByText('review a', {exact:true}).waitFor();
  const supported = await exercise(direct, ws, 'authorized direct');
  assert.equal(supported.read, true);
  assert.equal(supported.dispatched, true);

  const unrelated = await browser.newPage();
  await unrelated.goto(`http://127.0.0.1:${attacker.address().port}`);
  const denied = await exercise(unrelated, ws, 'unauthorized cross-origin');
  assert.deepEqual(denied, {opened:false, frames:0, read:false, dispatched:false});

  // The real dev proxy must not launder an unrelated website's Origin.
  const proxyWs = `ws://127.0.0.1:${process.env.DEV_PORT}/ws`;
  const proxyDenied = await exercise(unrelated, proxyWs, 'unauthorized proxy');
  assert.deepEqual(proxyDenied, denied);

  await unrelated.goto('data:text/html,<title>Opaque origin</title>');
  assert.equal(await unrelated.evaluate(() => location.origin), 'null');
  assert.deepEqual(await exercise(unrelated, ws, 'unauthorized null'), denied);

  const dev = await browser.newPage();
  await dev.goto(`http://127.0.0.1:${process.env.DEV_PORT}`);
  await dev.getByText('review a', {exact:true}).waitFor();
  const proxied = await exercise(dev, proxyWs, 'authorized proxy');
  assert.equal(proxied.read, true);
  assert.equal(proxied.dispatched, true);
  console.log('Chromium: direct UI and Vite read/write succeed; unrelated and null origins cannot connect, read, or mutate.');
} finally {
  if (browser) await browser.close();
  await vite.close();
  await new Promise(resolve => attacker.close(resolve));
}
";
