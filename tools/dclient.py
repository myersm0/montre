#!/usr/bin/env python3
"""
montre-daemon interactive client.

Connects to a running daemon, auto-registers, and drops to a REPL.

usage:
    python3 dclient.py [--socket /path/to/sock]

defaults to /tmp/montre-daemon.sock (matches what `cargo run --example serve_local`
sets up by default).

REPL commands:
    method                       send method with no params
    method {"k": "v"}            send method with JSON params
    .help                        show available methods
    .quit / exit / EOF           disconnect

examples:
    daemon> corpus.info
    daemon> corpus.documents
    daemon> corpus.documents {"component": "fr"}
    daemon> corpus.layer_info {"layer": "upos"}
    daemon> text.surface {"start": 0, "end": 5}
    daemon> alignment.list
    daemon> query.execute_count {"cql": "[pos=\\"NOUN\\"]"}
    daemon> query.execute {"cql": "[pos=\\"NOUN\\"]"}
    daemon> query.hits {"handle": "r-...", "offset": 0, "limit": 10}
"""

import argparse
import json
import socket
import struct
import sys


METHODS = [
	"corpus.info",
	"corpus.documents",
	"corpus.layer_info",
	"text.surface",
	"text.sentence",
	"text.sentences",
	"text.document",
	"text.annotations",
	"text.annotations_range",
	"alignment.list",
	"alignment.project",
	"query.execute",
	"query.execute_count",
	"query.hits",
	"query.metadata",
]


def send_frame(sock, payload):
	data = json.dumps(payload).encode("utf-8")
	sock.sendall(struct.pack(">I", len(data)))
	sock.sendall(data)


def recv_n(sock, n):
	buf = b""
	while len(buf) < n:
		chunk = sock.recv(n - len(buf))
		if not chunk:
			raise ConnectionError("daemon disconnected")
		buf += chunk
	return buf


def recv_frame(sock):
	header = recv_n(sock, 4)
	length = struct.unpack(">I", header)[0]
	return json.loads(recv_n(sock, length))


def call(sock, request_id, method, params=None):
	req = {"jsonrpc": "2.0", "id": request_id, "method": method}
	if params is not None:
		req["params"] = params
	send_frame(sock, req)
	return recv_frame(sock)


def parse_line(line):
	parts = line.split(maxsplit=1)
	method = parts[0]
	if len(parts) == 1:
		return method, None
	params_str = parts[1].strip()
	if not params_str:
		return method, None
	try:
		return method, json.loads(params_str)
	except json.JSONDecodeError as e:
		raise ValueError(f"bad params JSON: {e}")


def repl(sock):
	next_id = 1
	while True:
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
			for m in METHODS:
				print(f"  {m}")
			continue

		try:
			method, params = parse_line(line)
		except ValueError as e:
			print(e)
			continue

		try:
			reply = call(sock, next_id, method, params)
		except ConnectionError as e:
			print(f"connection lost: {e}")
			break
		next_id += 1
		print(json.dumps(reply, indent=2))


def main():
	parser = argparse.ArgumentParser(
		description="interactive montre-daemon client",
		formatter_class=argparse.RawDescriptionHelpFormatter,
		epilog=__doc__,
	)
	parser.add_argument("--socket", default="/tmp/montre-daemon.sock")
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

	reply = call(
		sock,
		0,
		"session.register",
		{"protocol_version": 1, "kind": "external"},
	)
	if "error" in reply:
		print(f"register failed: {reply['error']}")
		sys.exit(1)
	result = reply["result"]
	print(f"registered as process_id={result['process_id']}, "
		f"server_version={result['server_version']}, "
		f"daemon_epoch={result['daemon_epoch']}")
	print("type .help for methods, .quit to exit")

	repl(sock)
	sock.close()


if __name__ == "__main__":
	main()
