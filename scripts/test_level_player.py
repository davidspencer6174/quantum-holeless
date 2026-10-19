import tempfile
from pathlib import Path
import unittest

from level_player import Records, parse_record


RECORD = """id\tprefix\tstatus\tbest_steps
1\tUUULLDDDDR\trunning\t3

steps=3 solution=RRR
witness=concrete
Input:
exit 4,1
steps 3
42030

.... ...R .... .... ....
"""


class RecordTests(unittest.TestCase):
    def test_directional_push_and_exit(self):
        level = parse_record(RECORD)
        self.assertEqual(level["boxes"], [{"position": 1, "mask": 8}])
        self.assertEqual(level["exit"], 3)
        self.assertEqual(level["solution"], "RRR")

    def test_wrong_mask_and_covered_exit_rejected(self):
        with self.assertRaisesRegex(ValueError, "illegal push"):
            parse_record(RECORD.replace("...R", "U..."))
        with self.assertRaisesRegex(ValueError, "Exit starts covered"):
            parse_record(RECORD.replace("exit 4,1", "exit 2,1"))

    def test_quantum_family_skipped(self):
        self.assertIsNone(parse_record(RECORD.replace("witness=concrete", "witness=quantum-family")))

    def test_partial_write_retains_last_valid_record(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            (directory / "subtrees").mkdir()
            path = directory / "subtrees" / "root-00000001.txt"
            path.write_text(RECORD)
            records = Records(directory)
            self.assertEqual(len(records.snapshot()["levels"]), 1)
            path.write_text("id\tprefix\tstatus\tbest_steps\n")
            result = records.snapshot()
            self.assertEqual(len(result["levels"]), 1)
            self.assertEqual(len(result["warnings"]), 1)
            path.write_text(RECORD.replace("running", "finished"))
            self.assertEqual(records.snapshot()["levels"][0]["status"], "finished")


if __name__ == "__main__":
    unittest.main()
