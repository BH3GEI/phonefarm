#!/usr/bin/env python3
"""Navigate an existing gray-control owner; quality evidence stays with that owner."""
import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
from urllib.parse import urlsplit


PROFILE = "phonefarm/genshin-quality-profile@1"
RESULT = "phonefarm/genshin-quality-client@1"
ACTION = "phonefarm/gray-control-action@1"
RECEIPT = "phonefarm/gray-control-action-result@1"
QUALITY_ROOT = "gray-control/quality"
POINTS = ("menu", "clock", "confirm", "close_clock", "back_world")
MODES = ("probe", "target", "passdump", "loadop", "frameaudit", "frameaudit-ts")
HELP = """
Requires an already-running gray-layer session and an explicitly verified profile.
begin starts while the character is standing in the world; it opens the clock.
After human inspection, finish drags the clock, records a checkpoint, confirms,
returns to the world and records the final image/marker. Neither command starts,
restarts, installs, stops, or restores the owner. The caller is responsible for
the initial scene; dimensions/rotation alone cannot establish that scene.

Remote: begin passdump LABEL --job JOB --node-url URL --profile FILE --actor-id agent
Local:  begin passdump LABEL --session-out DIR --profile FILE --actor-id agent
Finish: finish LABEL <same locator/profile/actor options> --swipe X1 Y1 X2 Y2
SWIPE="X1 Y1 X2 Y2" is also accepted. Swipe duration is the historical 2500 ms.
--node-url may be replaced by an explicitly set PF_NODE_URL; no production URL
is assumed. Credentials are handled only by pf-node. PHONEFARM_BIN/PF_NODE_BIN
or --phonefarm/--pf-node select executables, not shell command strings.

Profile JSON has exactly: schema="phonefarm/genshin-quality-profile@1", package,
width, height, rotation (0..3), points. points contains menu, clock, confirm,
close_clock and back_world, each [x,y] in the ORIGINAL image coordinates.
Supply measured dimensions and deliberate coordinates; no default profile or
coordinate scaling is provided. Label is 1..48 ASCII letters/digits/_/-.

Every input uses a fresh capture and exact profile geometry. Actor/epoch are
fixed at invocation start; a takeover, even followed by return, stops this
client. Requests use gq-LABEL-b00/f00 onward, with b99/f99 for final quality.
stderr records the profile and each complete request before its single POST.
Redirect stderr to keep this client log. Timeouts only query that original ID
and exit. A partial prior invocation is NEVER resumed or replayed here: inspect
its receipts and continue deliberately using Web/generic session actions.
finish requires latest_quality to be this label in phase begun; historical or
partial flows also use Web/generic actions. An already completed invocation
only returns its original result, including after the owner has stopped.

Owner artifacts remain under gray-control/quality/LABEL. begin corresponds to
clock_before, checkpoint to clock_after, finish to the world image. Each stage
keeps original.png and derived RGB stride2 preview-half.png separately. No .raw
files or legacy shot.png are synthesized, and nothing is downloaded by this
client. Collected evidence does not prove matching scenes, quality, or recovery.
"""


class Failure(Exception):
    def __init__(self, code, message, **details):
        super().__init__(message)
        self.code, self.details = code, details


def emit(value, *, stream=sys.stdout):
    print(json.dumps(value, ensure_ascii=False), file=stream, flush=True)


def integer(value, low, high):
    return type(value) is int and low <= value <= high


def stamp(value):
    if not isinstance(value, str):
        raise Failure("invalid_response", "missing timestamp")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            raise ValueError("timestamp has no timezone")
        return parsed.timestamp()
    except ValueError as error:
        raise Failure("invalid_response", "invalid timestamp") from error


def load_profile(path):
    try:
        with open(path, "rb") as source:
            raw = source.read(65537)
        if len(raw) > 65536:
            raise ValueError("profile exceeds 64 KiB")
        profile = json.loads(raw)
    except (OSError, ValueError) as error:
        raise Failure("invalid_profile", str(error)) from error
    if (not isinstance(profile, dict)
            or set(profile) != {"schema", "package", "width", "height", "rotation", "points"}
            or profile["schema"] != PROFILE
            or not isinstance(profile["package"], str)
            or not re.fullmatch(r"[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)+", profile["package"])
            or not integer(profile["width"], 1, 8192)
            or not integer(profile["height"], 1, 8192)
            or not integer(profile["rotation"], 0, 3)
            or not isinstance(profile["points"], dict)
            or set(profile["points"]) != set(POINTS)):
        raise Failure("invalid_profile", "profile must match the documented schema and measured geometry")
    for name, point in profile["points"].items():
        if (not isinstance(point, list) or len(point) != 2
                or not integer(point[0], 0, profile["width"] - 1)
                or not integer(point[1], 0, profile["height"] - 1)):
            raise Failure("invalid_profile", f"invalid original-image point: {name}")
    return profile


class Client:
    def __init__(self, args):
        self.args = args
        self.env = os.environ.copy()
        if args.job:
            if not re.fullmatch(r"[0-9a-f]{12}", args.job):
                raise Failure("invalid_locator", "job must be 12 lowercase hexadecimal characters")
            url = args.node_url or os.environ.get("PF_NODE_URL")
            if not url:
                raise Failure("invalid_locator", "remote mode requires --node-url or explicit PF_NODE_URL")
            parts = urlsplit(url)
            if parts.scheme not in ("https", "http") or not parts.hostname or parts.username or parts.password or parts.query or parts.fragment:
                raise Failure("invalid_locator", "node URL must be an explicit HTTP(S) endpoint without embedded credentials/query")
            self.env["PF_NODE_URL"] = url.rstrip("/")
        elif args.node_url:
            raise Failure("invalid_locator", "--node-url is only valid with --job")

    def invoke(self, command, request=None):
        try:
            result = subprocess.run(command, input=None if request is None else json.dumps(request),
                                    text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    env=self.env, timeout=40)
        except subprocess.TimeoutExpired as error:
            raise Failure("tool_timeout", "client command timed out; its outcome is not inferred") from error
        except OSError as error:
            raise Failure("tool_unavailable", str(error)) from error
        try:
            value = json.loads(result.stdout)
            if not isinstance(value, dict):
                raise ValueError("not an object")
        except ValueError as error:
            raise Failure("invalid_response", "tool did not return a JSON object; stderr is not parsed as status") from error
        return result.returncode, value

    def decode(self, code, value, *, missing=False):
        if code == 0:
            return value
        if self.args.job:
            detail = value.get("error")
            if not isinstance(detail, dict):
                raise Failure("invalid_response", "remote error lacks structured error data")
            if missing and value.get("http_status") == 404 and detail.get("code") == "not_found":
                return None
            raise Failure("status_unavailable", str(detail.get("message", "remote request failed")),
                          remote_code=detail.get("code"), http_status=value.get("http_status"))
        if (missing and code == 2 and value.get("schema") == "phonefarm/gray-control-error@1"
                and value.get("code") == "not_found"):
            return None
        raise Failure("status_unavailable", str(value.get("error", "local request failed")),
                      local_code=value.get("code"))

    def status(self, request_id=None):
        if self.args.job:
            command = [self.args.pf_node, "session-status", self.args.job]
        else:
            command = [self.args.phonefarm, "gray-layer", "session-status", "--out", self.args.session_out]
        if request_id:
            command += ["--request-id", request_id]
        command += ["--json"]
        code, value = self.invoke(command)
        return self.decode(code, value, missing=request_id is not None)

    def post(self, request):
        command = ([self.args.pf_node, "session-action", self.args.job]
                   if self.args.job else
                   [self.args.phonefarm, "gray-layer", "session-action", "--out", self.args.session_out])
        command += ["--request", "-"]
        if not self.args.job:
            command += ["--json"]
        code, value = self.invoke(command, request)
        if code != 0:
            raise Failure("action_unconfirmed", "action client returned nonzero; only its original receipt may resolve it")
        return value

    def label_exists(self, label):
        if not self.args.job:
            fds = []
            try:
                for path in (self.args.session_out, "gray-control", "quality"):
                    fds.append(os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                       dir_fd=fds[-1] if fds else None))
                try:
                    os.stat(label, dir_fd=fds[-1], follow_symlinks=False)
                    return True
                except FileNotFoundError:
                    return False
            except OSError as error:
                raise Failure("artifacts_unavailable", "cannot inspect the existing quality directory") from error
            finally:
                for fd in reversed(fds):
                    os.close(fd)
        cursor, seen = None, set()
        for _ in range(20):
            command = [self.args.pf_node, "artifacts", self.args.job, "--path", QUALITY_ROOT, "--limit", "100"]
            if cursor:
                command += ["--cursor", cursor]
            code, value = self.invoke(command)
            if (code != 0 or value.get("schema_version") != 1 or value.get("job") != self.args.job
                    or value.get("path") != QUALITY_ROOT or not isinstance(value.get("items"), list)):
                raise Failure("artifacts_unavailable", "quality directory listing failed; it is not treated as empty")
            for item in value["items"]:
                if not isinstance(item, dict) or not isinstance(item.get("name"), str):
                    raise Failure("artifacts_unavailable", "invalid directory entry")
                if item["name"] == label:
                    return True
            cursor = value.get("next_cursor")
            if cursor is None:
                return False
            if not isinstance(cursor, str) or not cursor or cursor in seen:
                raise Failure("artifacts_unavailable", "invalid/repeated directory cursor")
            seen.add(cursor)
        raise Failure("artifacts_unavailable", "quality directory exceeds the bounded metadata scan")


class Invocation:
    def __init__(self, args, profile, client):
        self.args, self.profile, self.client = args, profile, client
        self.initial = client.status()
        descriptor = self.initial
        if (descriptor.get("schema") != "phonefarm/gray-control@1" or descriptor.get("producer") != "phonefarm"
                or not isinstance(descriptor.get("session_id"), str)
                or descriptor.get("package") != profile["package"]
                or (args.job and descriptor.get("job_id") != args.job)):
            raise Failure("session_mismatch", "descriptor does not identify the requested session/package")
        if args.command == "begin" and descriptor.get("mode") != args.mode:
            raise Failure("session_mismatch", "begin mode must match the already-installed owner mode")
        self.session_id = descriptor["session_id"]
        controller = descriptor.get("controller", {})
        self.epoch = controller.get("epoch")
        if not integer(self.epoch, 0, 2**64 - 1):
            raise Failure("invalid_response", "descriptor lacks a valid control epoch")
        self.prefix = f"gq-{args.label}-{'b' if args.command == 'begin' else 'f'}"
        self.last_request = None

    def request_id(self, step):
        return self.prefix + step

    def receipt(self, request_id):
        value = self.client.status(request_id)
        if value is not None:
            self.validate_receipt(value, request_id)
        return value

    def validate_receipt(self, receipt, request_id):
        if (receipt.get("schema") != RECEIPT or receipt.get("session_id") != self.session_id
                or receipt.get("request_id") != request_id):
            raise Failure("invalid_response", "action receipt identity mismatch", request_id=request_id)

    def guard(self, minimum_seconds=0):
        until = time.monotonic() + 2
        while True:
            d = self.client.status()
            if (d.get("session_id") != self.session_id or d.get("package") != self.profile["package"]
                    or d.get("mode") != self.initial.get("mode")):
                raise Failure("session_changed", "session identity changed; no further actions sent")
            controller = d.get("controller", {})
            if controller.get("actor_id") != self.args.actor_id or controller.get("epoch") != self.epoch:
                raise Failure("control_changed", "actor/epoch changed; this invocation does not adopt new control")
            if controller.get("expires_at") is not None and stamp(controller["expires_at"]) <= time.time():
                raise Failure("control_changed", "controller TTL has expired")
            if (d.get("live_owner") is not True or d.get("accepting") is not True
                    or d.get("action_capabilities", {}).get("quality") != 1
                    or not {"quality", "capture", "tap", "swipe"}.issubset(d.get("allowed_actions", []))):
                raise Failure("owner_unavailable", "owner must be live and support quality/capture/input")
            if stamp(d.get("deadline_at")) <= time.time() + minimum_seconds:
                raise Failure("budget_exhausted", "session budget does not cover the next step/wait")
            if d.get("phase") == "ready":
                return d
            # A completed receipt is published just before descriptor.ready.
            # Only observe this short publication gap; never wait through a
            # measurement/recovery phase or adopt a changed controller.
            if d.get("phase") != "executing" or time.monotonic() >= until:
                raise Failure("owner_unavailable", "owner did not become ready; no further actions sent")
            time.sleep(0.1)

    def submit(self, step, action, params):
        self.guard()
        request_id = self.request_id(step)
        self.last_request = request_id
        request = {"schema": ACTION, "request_id": request_id, "session_id": self.session_id,
                   "actor_id": self.args.actor_id, "control_epoch": self.epoch,
                   "action": action, "params": params}
        emit({"event": "request_before_post", "request": request}, stream=sys.stderr)
        try:
            receipt = self.client.post(request)
            self.validate_receipt(receipt, request_id)
        except Failure as error:
            try:
                observed = self.receipt(request_id)
            except Failure as lookup_error:
                observed = {"lookup_error": lookup_error.code}
            raise Failure("action_result_unknown", "POST outcome unconfirmed; queried only the original ID and stopped",
                          request_id=request_id, cause=error.code, observed_receipt=observed) from error
        deadline = time.monotonic() + 120
        while receipt.get("status") in ("accepted", "started"):
            if time.monotonic() >= deadline:
                raise Failure("action_result_unknown", "original request is still pending; stopped without replay",
                              request_id=request_id, observed_receipt=receipt)
            time.sleep(0.25)
            receipt = self.receipt(request_id)
            if receipt is None:
                raise Failure("action_result_unknown", "accepted request is no longer readable", request_id=request_id)
        if receipt.get("status") != "completed":
            raise Failure("action_failed", "action did not complete; navigation will not continue",
                          request_id=request_id, observed_receipt=receipt)
        return receipt.get("result")

    def image(self, result, request_id):
        image = result.get("image") if isinstance(result, dict) else None
        if (not isinstance(image, dict) or image.get("request_id") != request_id
                or any(type(image.get(k)) is not int or image[k] != self.profile[k]
                       for k in ("width", "height", "rotation"))):
            raise Failure("geometry_mismatch", "fresh original image does not exactly match the explicit profile",
                          request_id=request_id, observed_image=image)
        if not 0 <= time.time() - stamp(image.get("captured_at")) <= 30:
            raise Failure("stale_image", "fresh capture became stale before use", request_id=request_id)
        return image

    def capture(self, step):
        return self.image(self.submit(step, "capture", {}), self.request_id(step))

    def input(self, step, action, image, params):
        self.image({"image": image}, image["request_id"])
        d = self.guard()
        if (d.get("latest_image") or {}).get("request_id") != image["request_id"]:
            raise Failure("image_changed", "another capture replaced this frame; no input sent")
        return self.submit(step, action, {**params, "frame_id": image["request_id"]})

    def tap(self, capture_step, tap_step, point):
        image = self.capture(capture_step)
        x, y = self.profile["points"][point]
        self.input(tap_step, "tap", image, {"x": x, "y": y})

    def wait(self, seconds):
        self.guard(seconds)
        time.sleep(seconds)
        self.guard()

    def quality(self, step, op):
        request_id = self.request_id(step)
        result = self.submit(step, "quality", {"op": op, "label": self.args.label})
        self.image(result, request_id)
        quality = result.get("quality", {})
        expected = {"begin": "begun", "checkpoint": "checkpointed", "finish": "collected"}[op]
        if (result.get("collection", {}).get("status") != "complete" or quality.get("label") != self.args.label
                or quality.get("phase") != expected
                or quality.get("stages", {}).get(op, {}).get("request_id") != request_id):
            raise Failure("invalid_response", "completed quality receipt does not contain the requested stage", request_id=request_id)
        return result

    def run(self):
        first_id, final_id = self.request_id("00"), self.request_id("99")
        first, final = self.receipt(first_id), self.receipt(final_id)
        if first is not None or final is not None:
            result = (final or {}).get("result") or {}
            quality = result.get("quality") or {}
            expected = "begun" if self.args.command == "begin" else "collected"
            if (first is not None and final is not None and final.get("status") == "completed"
                    and result.get("collection", {}).get("status") == "complete"
                    and quality.get("label") == self.args.label and quality.get("phase") == expected):
                return self.output(result, repeated=True)
            raise Failure("partial_invocation", "this stage already started; inspect its receipts and continue deliberately with Web/generic actions",
                          first_request_id=first_id, final_request_id=final_id,
                          first_receipt=first, final_receipt=final)
        self.guard()
        if self.args.command == "begin":
            if self.client.label_exists(self.args.label):
                raise Failure("existing_label", "quality label already exists; navigation is not replayed")
        else:
            quality = self.initial.get("latest_quality") or {}
            if quality.get("label") != self.args.label or quality.get("phase") != "begun" or quality.get("active_attempt") is not None:
                raise Failure("quality_state", "finish requires this label to be the latest begun quality; use Web/generic actions for historical/partial flows")
        emit({"event": "invocation", "session_id": self.session_id, "job_id": self.initial.get("job_id"),
              "command": self.args.command, "label": self.args.label, "profile": self.profile,
              "actor_id": self.args.actor_id, "control_epoch": self.epoch}, stream=sys.stderr)
        if self.args.command == "begin":
            self.tap("00", "01", "menu")
            self.wait(3)
            self.tap("02", "03", "clock")
            self.wait(3)
            result = self.quality("99", "begin")
        else:
            image = self.capture("00")  # Never reuse the pre-human-wait frame.
            x1, y1, x2, y2 = self.args.swipe
            self.input("01", "swipe", image, {"x1": x1, "y1": y1, "x2": x2, "y2": y2, "duration_ms": 2500})
            self.wait(1)
            self.quality("02", "checkpoint")
            self.tap("03", "04", "confirm")
            self.wait(12)
            self.tap("05", "06", "close_clock")
            self.wait(3)
            self.tap("07", "08", "back_world")
            self.wait(8)
            result = self.quality("99", "finish")
        return self.output(result, repeated=False)

    def output(self, result, *, repeated):
        return {"schema": RESULT, "status": "completed", "command": self.args.command,
                "label": self.args.label, "session_id": self.session_id, "job_id": self.initial.get("job_id"),
                "repeated": repeated, "result": result,
                "artifact_roles": {"begin": "clock_before", "checkpoint": "clock_after", "finish": "world"},
                "scene_comparison": "not_evaluated"}


def arguments(argv):
    parser = argparse.ArgumentParser(description=__doc__, epilog=HELP, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("begin", "finish"):
        command = commands.add_parser(name, epilog=HELP, formatter_class=argparse.RawDescriptionHelpFormatter)
        if name == "begin":
            command.add_argument("mode", choices=MODES)
        command.add_argument("label")
        locator = command.add_mutually_exclusive_group(required=True)
        locator.add_argument("--job")
        locator.add_argument("--session-out")
        command.add_argument("--node-url")
        command.add_argument("--profile", required=True)
        command.add_argument("--actor-id", required=True)
        command.add_argument("--pf-node", default=os.environ.get("PF_NODE_BIN", "pf-node"))
        command.add_argument("--phonefarm", default=os.environ.get("PHONEFARM_BIN", "phonefarm"))
        if name == "finish":
            command.add_argument("--swipe", nargs=4, type=int, metavar=("X1", "Y1", "X2", "Y2"))
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,48}", args.label) or not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", args.actor_id):
        raise Failure("invalid_arguments", "label must be 1..48 and actor 1..64 ASCII letters/digits/_/-")
    if args.command == "finish" and args.swipe is None:
        raw = os.environ.get("SWIPE", "").split()
        if len(raw) != 4 or any(not re.fullmatch(r"[0-9]+", v) for v in raw):
            raise Failure("invalid_arguments", "finish requires --swipe X1 Y1 X2 Y2 or four numeric SWIPE values")
        args.swipe = list(map(int, raw))
    return args


def main(argv=None):
    invocation = None
    try:
        args = arguments(sys.argv[1:] if argv is None else argv)
        profile = load_profile(args.profile)
        if args.command == "finish" and any(not integer(value, 0, profile[axis] - 1)
                                            for value, axis in zip(args.swipe, ("width", "height", "width", "height"))):
            raise Failure("invalid_arguments", "swipe coordinates must fit the explicit original-image profile")
        invocation = Invocation(args, profile, Client(args))
        emit(invocation.run())
        return 0
    except Failure as error:
        emit({"schema": RESULT, "status": "partial" if error.code == "partial_invocation" else "failed",
              "error": {"code": error.code, "message": str(error)},
              "last_request_id": invocation.last_request if invocation else None, **error.details})
        return 2
    except KeyboardInterrupt:
        emit({"schema": RESULT, "status": "unknown", "error": {"code": "interrupted", "message": "client interrupted; owner was not cancelled; query the original ID before further input"},
              "last_request_id": invocation.last_request if invocation else None})
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
