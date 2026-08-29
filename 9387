import Fullstory from './fullstory';
domain=assistance.aid.com
declare global {
  interface Window {
    __wr_on_ready: () => void;
    _fs_ready: any;
  }
}

export interface WixRecorderOptions {
  sample?: number;
  smallApp?: boolean;
}

export interface WixRecorder {
  addLabel(label: string): Promise<void>;
  withExperiments(specs: { [specKey: string]: string }): Promise<void>;
  recordUrl(): Promise<string>;
  addCustomAttribute(key: string, value: string | string[]): Promise<void>;
  __forceRecording(): Promise<void>;
}

const options = getRecorderOptions();

const GENERIC_EVENT_NAME = 'wix recorder custom attribute';

const fullstory = new Fullstory({
  onReady: window.__wr_on_ready,
  sample: options.sample,
  smallApp: options.smallApp,
});

fullstory.setup();

export async function addLabel(label: string) {
  return fullstory.event(GENERIC_EVENT_NAME, { label });
}

export async function withExperiments(specs: {
  [specKey: string]: string;
}): Promise<void> {
  fullstory.event('Experiment', specs);
}

export async function addCustomAttribute(
  key: string,
  value: string | string[],
): Promise<void> {
  fullstory.event(GENERIC_EVENT_NAME, { [key]: value });
}

export async function recordUrl(): Promise<string> {
  return fullstory.getCurrentSessionUrl();
}

export async function __forceRecording(): Promise<void> {
  return fullstory.forceLoadFullstory();
}

function getRecorderOptions(): WixRecorderOptions {
  const opts = {} as WixRecorderOptions;

  try {
    const data = document?.currentScript?.dataset;

    if (data) {
      opts.sample =
        typeof data.sample === 'string' ? parseFloat(data.sample) : undefined;
      opts.smallApp = data.smallApp === 'true';
    }
  } catch {}

  return opts;
}

<!doctype html>
<html>
  <head>
    <title>Wix Site Creation | Wix.com</title>
    <link type="image/png" href="https://www.wix.com/favicon.ico" rel="shortcut icon">
      <link rel="stylesheet" href="//static.parastorage.com/services/funnel-intro-chat/1.2275.0/app.min.css">
    <meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1">

    <script src="https://static.parastorage.com/unpkg-semver/fedops-logger@5/fedops-logger.bundle.min.js"></script>
    <script crossorigin="anonymous" id="sentry" src="https://static.parastorage.com/unpkg/@sentry/browser@5.30.0/build/bundle.min.js"></script>

    <script>
      fedopsLogger.reportAppLoadStarted("funnel-intro-chat");
    </script>
    <script src="https://static.parastorage.com/unpkg/react@18.3.1/umd/react.production.min.js"></script>
    <script src="https://static.parastorage.com/unpkg/react-dom@18.3.1/umd/react-dom.production.min.js"></script>
   </head>
  <body>
    <div id="root"></div>

    
    <script>
      window.onWixRecorderLoaded = function () {
        try {
          window.wixRecorder.addLabel('funnel-intro-chat');
          window.wixRecorder.addLabel('chat-flow-' + new URLSearchParams(window.location.search).get('flow') || 'main')
          // do nothing intentionally
        } catch (_e) {}
      };
    </script>
    <script async src="https://static.parastorage.com/unpkg-semver/wix-recorder@^1/app.bundle.min.js"
      crossorigin onload="onWixRecorderLoaded()"></script>
    
    
    <script>
      window.__BASEURL__ = '/ai-assistant/';
      window.__LOCALE__ = 'en';
      window.__IS_MOBILE__ = 'false';
      window.__USER_ID__ = 'b3e617bc-e800-4198-8670-937149685b87';
      window.__USER_DISPLAY_NAME__ = 'Jazmyne Josephine';
      window.__USER_IMAGE__ = 'https://lh3.googleusercontent.com/a/ACg8ocKO7Cwru8Ct3FDhGnnssLsaujsAgwR7jZshQFjZOaZpnZlsAQ%3Ds96-c';
      window.__ARTIFACT_NAME__ = 'funnel-intro-chat';
      window.__ARTIFACT_VERSION__ = '1.2275.0';
      window.__SENTRY_DSN__ = 'https://b22e8eacef364954891949f17db822b2@sentry-next.wixpress.com/10465';
      
      window.__EXPERIMENTS__ = atob('eyJzcGVjcy5mdW5uZWwuQ2hhdEdlbmVyYXRlRG9uZU1lc3NhZ2UiOiJmYWxzZSIsImZ1bm5lbENoYXRMZWdhbEtlZXBlclNob3J0T3V0cHV0IjoiQSIsInByZW1pdW1Eb21haW5zRmxvd0luRnVubmVsIjoiQiIsInNwZWNzLmZ1bm5lbC5TZWN0b3JUb0FwcFYyIjoiZmFsc2UiLCJmdW5uZWxDaGF0U3VnZ2VzdGlvbnMiOiJBIiwiZnVubmVsQ2hhdEl0ZXJhdGlvbkF3YXJlSW5zaWdodHNFeHRyYWN0aW9uIjoiQSIsInNwZWNzLmZ1bm5lbC5kb21haW5XaWRnZXRHZXRJdERvbmUiOiJ0cnVlIiwic3BlY3MuZnVubmVsLkNoYXRTdHJlYW1pbmdSZXNwb25zZSI6InRydWUiLCJzcGVjcy5mdW5uZWwuZW5hYmxlTmV3SGFybW9ueURlc2lnbk1vYmlsZSI6InRydWUiLCJmdW5uZWxTZWN0b3JUb0FwcFYyIjoiQSIsInNwZWNzLmZ1bm5lbC5DaGF0TWVyZ2VFZGl0QXBwc0FuZERvbmUiOiJmYWxzZSIsImZ1bm5lbENoYXRKdXN0RXhwbG9yaW5nU2tpcEFiVHJhbnNsYXRlIjoiQSIsInNwZWNzLmZ1bm5lbC5lbmFibGVIYXJtb255UmVzdGF1cmFudHNPcmRlcnMiOiJ0cnVlIiwiZnVubmVsTmV3QnVzaW5lc3NEZXNjcmlwdGlvbiI6IkIiLCJzcGVjcy5mdW5uZWwuRnVubmVsSW50cm9DaGF0RG9tYWluc1N1Z2dlc3Rpb25zV2lkZ2V0IjoidHJ1ZSIsInNwZWNzLmZ1bm5lbC5DaGF0Qm9va2luZ3NBcHBOZXdEZXNjcmlwdGlvbiI6InRydWUiLCJmdW5uZWxDaGF0TW9iaWxlU3BsaXRTY3JlZW5Ta2lwQnV0dG9uIjoiQSIsImZ1bm5lbENoYXRFbmFibGVEb21haW5CdXR0b25zIjoiQiIsImZ1bm5lbENoYXRMYW5ndWFnZXNHZXRTdGFydGVkV2lkZ2V0S2V5c0FCVGVzdCI6IkEiLCJmdW5uZWxJbnRyb0NoYXRHZW5lcmF0ZUFsbEFwcHNXaWRnZXRDb250ZW50IjoiQSIsInNwZWNzLmZ1bm5lbC5DaGF0U3BsaXRDb250ZW50VXBkYXRlIjoidHJ1ZSIsInNwZWNzLmZ1bm5lbC5DaGF0TW9iaWxlQTExeU9wdGltaXphdGlvbnMiOiJ0cnVlIiwic3BlY3MuZnVubmVsLkNvbnRlbnRRdWVzdGlvbiI6ImZhbHNlIiwiZnVubmVsQ2hhdERvbWFpbk1vZGFsU2l0ZU5hbWUiOiJBIiwic3BlY3MuZnVubmVsLkFsaWduU3RvcmVzRXh0cmFjdGlvbiI6InRydWUiLCJzcGVjcy5mdW5uZWwuRG9tYWluc1N1Z2dlc3Rpb25zV2lkZ2V0RXhwYW5kIjoidHJ1ZSIsImZ1bm5lbENoYXROb3RFeHRyYWN0SW50ZW50QXBwcyI6IkEiLCJ0ZXN0SUQiOiJBIiwiZnVubmVsU2VydmljZXNBcHAiOiJBIiwiZnVubmVsQ2hhdExlZ2FsS2VlcGVyU2luZ2xlTWVzc2FnZSI6IkEiLCJzcGVjcy5mdW5uZWwuYmxvY2tBcHBzRm9yU3VwZXJCb3dsIjoidHJ1ZSIsImZ1bm5lbENoYXRXZWxjb21lRm9yQ2xpZW50IjoiQSIsInNwZWNzLmZ1bm5lbC5TZXJ2aWNlc0FwcCI6ImZhbHNlIiwiZnVubmVsQ2hhdFN1Z2dlc3Rpb25zTW9iaWxlIjoiQSIsImNoYXRHb1RvSGFybW9ueVNraXBEYXNoYm9hcmRGVCI6IkEiLCJmdW5uZWxDaGF0TWVyZ2VFZGl0QXBwc0FuZERvbmVNb2JpbGUiOiJBIiwiZnVubmVsSW50cm9DaGF0Q2F0ZWdvcnlTdWdnZXN0aW9uc01vYmlsZSI6IkEiLCJmdW5uZWxJbnRyb0NoYXRDYXRlZ29yeVN1Z2dlc3Rpb25zRGVza3RvcCI6IkEiLCJmdW5uZWxDaGF0TW9iaWxlSGFybW9ueVVJVjMiOiJBIiwiZnVubmVsQ29udmVyc2F0aW9uUHJvbXB0VjEwIjoiQiIsImZ1bm5lbENoYXRHb1RvSGFybW9ueVNraXBEYXNoYm9hcmQiOiJBIiwiZnVubmVsQ2hhdE5ld1dlbGNvbWVTY3JlZW4iOiJBIiwiZnVubmVsQ2hhdEZlZWRiYWNrRm9ybSI6IkEiLCJmdW5uZWxDaGF0RG9tYWluV2lkZ2V0T25FYXJseUV4aXQiOiJBIiwic3BlY3MuZnVubmVsLmVuYWJsZU5ld0hhcm1vbnlEZXNpZ24iOiJ0cnVlIiwic3BlY3MuZnVubmVsLkJ1c2luZXNzVGVybVJhbmtpbmdQcm9tcHRWNC4yIjoidHJ1ZSIsImZ1bm5lbENoYXREb21haW5XaWRnZXRBbm90aGVyRG9tYWluIjoiQSIsInNwZWNzLmZ1bm5lbC5MYW5ndWFnZXNHZXRTdGFydGVkV2lkZ2V0S2V5c0FCVGVzdCI6ImZhbHNlIiwic3BlY3MuZnVubmVsLk1vYmlsZU5ld1N0YXJ0U2NyZWVuIjoidHJ1ZSIsImZ1bm5lbENoYXREb21haW5JblNpdGVQcm9maWxlIjoiQSIsInNwZWNzLm9kZWRpdG9yLnN0YXJ0UGFnZU5ld1VpIjoiZmFsc2UifQ==')
    </script>

    <script src="//static.parastorage.com/services/funnel-intro-chat/1.2275.0/app.bundle.min.js"></script>
  </body>
</html>
