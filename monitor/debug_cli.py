"""
IPC 调试工具
用于通过命令行与后台监控进程进行交互，发送调试指令。
"""

import sys
import json
import time
import os
import shutil
import argparse
from monitor.ipc_pipe import send_command


def main():
    parser = argparse.ArgumentParser(description="Carbon Paper Monitor IPC Debug Tool")
    parser.add_argument("--pipe", help="Pipe name (override env CARBON_MONITOR_PIPE)")

    # Automatic pipe name reading
    parser.add_argument(
        "--auto",
        action="store_true",
        help="Automatically read pipe name from debug file. (If debug mode in main file is enabled)",
    )

    subparsers = parser.add_subparsers(dest="command", help="Available commands")

    subparsers.add_parser("drop", help="Drop the monitor database")

    subparsers.add_parser("status", help="Get monitor status")

    subparsers.add_parser("stop", help="Stop the monitor process")

    subparsers.add_parser("pause", help="Pause screen capture")

    subparsers.add_parser("resume", help="Resume screen capture")

    # Search command
    search_parser = subparsers.add_parser("search", help="Search OCR text in database")
    search_parser.add_argument("query", help="Text to search for")
    search_parser.add_argument(
        "--limit", type=int, default=10, help="Max results (default: 10)"
    )

    # Natural Language Search command
    nl_search_parser = subparsers.add_parser(
        "search_nl", help="Search screenshots using natural language description"
    )
    nl_search_parser.add_argument("query", help="Description to search for")
    nl_search_parser.add_argument(
        "--limit", type=int, default=10, help="Max results (default: 10)"
    )

    args = parser.parse_args()

    if not args.command:
        parser.print_help()
        sys.exit(1)

    if args.command == "drop":
        confirm = input(
            "Are you sure you want to DROP the monitor database? This action cannot be undone! (yes/no): "
        )
        if confirm.lower() != "yes":
            print("Aborted dropping the database.")
            sys.exit(0)
        
        if os.path.exists("ocr_data.db"):
            os.remove("ocr_data.db")
            print("Monitor database dropped.")
        else:
            print("No monitor database found to drop.")

        try:
            shutil.rmtree("screenshots", ignore_errors=True)
            print("Screenshots directory removed.")
        except FileNotFoundError as e: 
            print("No screenshots directory found to remove.")
            pass

        try:
            shutil.rmtree("chroma_db", ignore_errors=True)
            print("Chroma DB directory removed.")
        except FileNotFoundError as e:
            print("No Chroma DB directory found to remove.")
            pass
        finally:
            os.mkdir("chroma_db")

        sys.exit(0)

    # Get pipe name
    pipe_name = args.pipe or os.environ.get("CARBON_MONITOR_PIPE")
    if not pipe_name:
        # Try to find a default random pipe name from a temporary file or specific convention
        # For now, just ask user to provide it if env var is missing
        if args.auto:
            try:
                with open("monitor_pipe_name.txt", "r", encoding="utf-8") as f:
                    pipe_name = f.read().strip()
            except Exception as e:
                print("Error: Unable to read pipe name from debug file:", e)
                sys.exit(1)
        else:
            print(
                "Error: PIPE name not found. Please set CARBON_MONITOR_PIPE env var or use --pipe."
            )
            sys.exit(1)

    print(f"Connecting to pipe: {pipe_name}")

    # Construct payload
    payload = {"command": args.command}

    if args.command == "search":
        payload["query"] = args.query
        payload["limit"] = args.limit
    elif args.command == "search_nl":
        payload["query"] = args.query
        payload["limit"] = args.limit

    # Send command
    try:
        start_time = time.time()
        response = send_command(pipe_name, payload)
        duration = time.time() - start_time

        print(f"\nResponse ({duration:.3f}s):")
        print(json.dumps(response, indent=2, ensure_ascii=False))

    except Exception as e:
        print(f"Error communicating with monitor: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
