with open("../run_output_debug.txt", "r", encoding="utf-8", errors="ignore") as f:
    for i, line in enumerate(f, 1):
        if "[TRACE]" in line or "Result:" in line:
            print(f"{i}: {line.strip()}")
