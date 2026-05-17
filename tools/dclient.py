#!/usr/bin/env python3
"""
montre-daemon interactive client.

Connects to a running daemon, auto-registers, and drops to a REPL with
asynchronous notification handling.

usage:
    python3 dclient.py [--socket /path/to/sock]
                       [--kind KIND] [--label LABEL]
                       [--provides KINDS] [--consumes KINDS]

defaults to /tmp/montre-daemon.sock (matches `cargo run --example serve_local`).

registration flags control how this client registers with the daemon:
    --kind        ProcessKind (default: external)
    --label       optional roster label
    --provides    comma-separated InterestKinds this process publishes
    --consumes    comma-separated InterestKinds this process consumes

to exercise coupler behavior end-to-end, launch two instances: a follower
with the InterestKinds it consumes, and a master that provides them.
example:
    # terminal 1 (follower)
    python3 dclient.py --kind reader --consumes sentence

    # terminal 2 (master)
    python3 dclient.py --kind kwic --provides hit
    daemon> coupler.create {"master_id": 2, "follower_id": 1, "kind": {"type": "sentence_mirror"}}
    daemon> .notify session.publish_interest {"interest": {"type": "hit", ...}}

REPL commands:
    method                       send method as a request (waits for response)
    method {"k": "v"}            same, with JSON params
    .notify method [params]      send as a notification (no id, no reply expected)
    .help                        list available methods
    .quit / exit / EOF           disconnect

server-pushed notifications (coupler_update, roster_changed,
named_results_changed, shutdown) print as they arrive.
"""

import argparse
import json
import queue
import socket
import struct
import sys
import threading


methods = [
	"session.unregister",
	"session.update_label",
	"session.roster",
	"session.publish_interest (notification)",
	"corpus.info",
	"corpus.documents",
	"corpus.layer_info",
	"text.surface",
	"text.sentence",
	"text.sentences",
	"text.document",
	"text.annotations",
	"text.annotations_range",
	"query.execute",
	"query.execute_count",
	"query.hits",
	"query.metadata",
	"query.save",
	"query.materialize",
	"query.load",
	"query.list_named",
	"query.delete_named",
	"query.discard",
	"alignment.list",
	"alignment.project",
	"coupler.create",
	"coupler.remove",
	"coupler.list",
	"subscription.subscribe",
	"subscription.unsubscribe",
	"daemon.shutdown",
]


def send_frame(sock, payload):
	data = json.dumps(payload).encode("utf-8")
	sock.sendall(struct.pack(">I", len(data)))
	sock.sendall(data)


def recv_n(sock, n):
	buffer = b""
	while len(buffer) < n:
		chunk = sock.recv(n - len(buffer))
		if not chunk:
			raise ConnectionError("daemon disconnected")
		buffer += chunk
	return buffer


def recv_frame(sock):
	header = recv_n(sock, 4)
	length = struct.unpack(">I", header)[0]
	return json.loads(recv_n(sock, length))


def reader_loop(sock, responses, stop_flag):
	while not stop_flag.is_set():
		try:
			frame = recv_frame(sock)
		except (ConnectionError, OSError, ValueError) as e:
			if not stop_flag.is_set():
				sys.stdout.write(f"\n[connection closed: {e}]\n")
				sys.stdout.flush()
			stop_flag.set()
			break
		if "id" in frame:
			responses.put(frame)
		else:
			method = frame.get("method", "?")
			params = frame.get("params", {})
			sys.stdout.write(f"\n[notification: {method}]\n")
			sys.stdout.write(json.dumps(params, indent=2))
			sys.stdout.write("\ndaemon> ")
			sys.stdout.flush()


def send_request(sock, responses, request_id, method, params):
	request = {"jsonrpc": "2.0", "id": request_id, "method": method}
	if params is not None:
		request["params"] = params
	send_frame(sock, request)
	return responses.get(timeout=30)


def send_notification(sock, method, params):
	request = {"jsonrpc": "2.0", "method": method}
	if params is not None:
		request["params"] = params
	send_frame(sock, request)


def parse_method_and_params(text):
	parts = text.split(maxsplit=1)
	if not parts or not parts[0]:
		raise ValueError("missing method name")
	method = parts[0]
	if len(parts) == 1:
		return method, None
	params_text = parts[1].strip()
	if not params_text:
		return method, None
	try:
		return method, json.loads(params_text)
	except json.JSONDecodeError as e:
		raise ValueError(f"bad params JSON: {e}\n  input: {params_text}")


def parse_command(line):
	if line.startswith(".notify"):
		method, params = parse_method_and_params(line[len(".notify"):].strip())
		return "notify", method, params
	method, params = parse_method_and_params(line)
	return "request", method, params


def repl(sock, responses, stop_flag):
	next_id = 1
	while not stop_flag.is_set():
		try:
			line = input("daemon> ").strip()
		except (EOFError, KeyboardInterrupt):
			print()
			break
		if not line:
			continue
		if line in (".quit", "exit", "quit"):
			break
		if line == ".help":
			print("available methods:")
			for m in methods:
				print(f"  {m}")
			print("\nuse '.notify METHOD [PARAMS]' to send a notification (no reply expected)")
			continue

		try:
			kind, method, params = parse_command(line)
		except ValueError as e:
			print(e)
			continue

		try:
			if kind == "notify":
				send_notification(sock, method, params)
				print("(sent as notification, no reply)")
			else:
				reply = send_request(sock, responses, next_id, method, params)
				next_id += 1
				print(json.dumps(reply, indent=2))
		except ConnectionError as e:
			print(f"connection lost: {e}")
			break
		except queue.Empty:
			print("timeout waiting for response")
			break


def main():
	parser = argparse.ArgumentParser(
		description="interactive montre-daemon client",
		formatter_class=argparse.RawDescriptionHelpFormatter,
		epilog=__doc__,
	)
	parser.add_argument("--socket", default="/tmp/montre-daemon.sock")
	parser.add_argument(
		"--kind",
		default="external",
		choices=["external", "reader", "kwic", "conllu", "docs", "vocab", "results"],
		help="ProcessKind to register as (default: external)",
	)
	parser.add_argument(
		"--label",
		help="optional process label shown in roster",
	)
	parser.add_argument(
		"--provides",
		default="",
		help="comma-separated InterestKinds published by this process "
			"(position, span, sentence, hit, results, document)",
	)
	parser.add_argument(
		"--consumes",
		default="",
		help="comma-separated InterestKinds consumed by this process",
	)
	args = parser.parse_args()

	sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	try:
		sock.connect(args.socket)
	except FileNotFoundError:
		print(f"socket not found at {args.socket}")
		print("start the daemon first:")
		print("    cargo run --example serve_local -p montre-daemon -- /path/to/corpus")
		sys.exit(1)
	except ConnectionRefusedError:
		print(f"stale socket at {args.socket} (no listener)")
		print("remove it and restart the daemon")
		sys.exit(1)

	responses = queue.Queue()
	stop_flag = threading.Event()
	reader = threading.Thread(
		target=reader_loop,
		args=(sock, responses, stop_flag),
		daemon=True,
	)
	reader.start()

	register_params = {"protocol_version": 1, "kind": args.kind}
	if args.label:
		register_params["label"] = args.label
	if args.provides:
		register_params["provides"] = [k.strip() for k in args.provides.split(",") if k.strip()]
	if args.consumes:
		register_params["consumes"] = [k.strip() for k in args.consumes.split(",") if k.strip()]

	try:
		reply = send_request(
			sock, responses, 0,
			"session.register",
			register_params,
		)
	except (ConnectionError, queue.Empty) as e:
		print(f"register failed: {e}")
		sys.exit(1)

	if "error" in reply:
		print(f"register failed: {reply['error']}")
		sys.exit(1)
	result = reply["result"]
	print(f"registered as process_id={result['process_id']}, "
		f"server_version={result['server_version']}, "
		f"daemon_epoch={result['daemon_epoch']}")
	print("type .help for methods, .quit to exit")

	repl(sock, responses, stop_flag)
	stop_flag.set()
	sock.close()


if __name__ == "__main__":
	main()
