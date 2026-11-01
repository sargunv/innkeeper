// Host tests for ets/ime/TextSession.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const ts = await load("TextSession");

class Recorder {
  calls = [];
  insertText(t) {
    this.calls.push(["insert", t]);
  }
  deleteBefore(n) {
    this.calls.push(["before", n]);
  }
  deleteAfter(n) {
    this.calls.push(["after", n]);
  }
  setPreview(t) {
    this.calls.push(["preview", t]);
  }
  clearPreview() {
    this.calls.push(["clear"]);
  }
}

function batch(session, ed, lines) {
  for (const l of lines) assert.ok(session.onLine(l, ed), l);
}

test("content types follow the editor's pattern", () => {
  const line = (p, enter = 0, cap = 0) => ts.contentLine(p, enter, cap);
  assert.equal(line(ts.PATTERN_TEXT), "ti_content 0 0");
  assert.equal(
    line(ts.PATTERN_TEXT, ts.ENTER_KEY_TYPE_NEWLINE),
    "ti_content 512 0",
  );
  assert.equal(
    line(ts.PATTERN_TEXT, 0, ts.CAPITALIZE_SENTENCES),
    "ti_content 4 0",
  );
  assert.equal(
    line(ts.PATTERN_TEXT, 0, ts.CAPITALIZE_WORDS),
    "ti_content 32 0",
  );
  assert.equal(
    line(ts.PATTERN_TEXT, 0, ts.CAPITALIZE_CHARACTERS),
    "ti_content 16 0",
  );
  assert.equal(line(ts.PATTERN_NUMBER), "ti_content 0 2");
  assert.equal(line(ts.PATTERN_NUMBER_DECIMAL), "ti_content 0 3");
  assert.equal(line(ts.PATTERN_PHONE), "ti_content 0 4");
  assert.equal(line(ts.PATTERN_URI), "ti_content 0 5");
  assert.equal(line(ts.PATTERN_EMAIL), "ti_content 0 6");
  assert.equal(line(ts.PATTERN_DATETIME), "ti_content 0 12");
  for (const p of [
    ts.PATTERN_PASSWORD,
    ts.PATTERN_PASSWORD_SCREEN_LOCK,
    ts.PATTERN_NEW_PASSWORD,
  ]) {
    assert.equal(
      line(p, ts.ENTER_KEY_TYPE_NEWLINE, ts.CAPITALIZE_WORDS),
      "ti_content 192 8",
    );
  }
  assert.equal(line(ts.PATTERN_PASSWORD_NUMBER), "ti_content 192 9");
  assert.equal(line(ts.PATTERN_ONE_TIME_CODE), "ti_content 128 2");
  assert.equal(line(ts.PATTERN_USER_NAME), "ti_content 0 0");
  assert.equal(
    line(ts.PATTERN_NULL, ts.ENTER_KEY_TYPE_NEWLINE),
    "ti_content 512 0",
  );
});

test("the cursor line has the cursor's top and height", () => {
  assert.equal(ts.cursorLine(10.4, 99.6, 0.2), "ti_cursor 10 100 1 1");
  assert.equal(ts.cursorLine(10, 20, 30), "ti_cursor 10 20 1 30");
});

test("text is percent-encoded as the bridge expects", () => {
  assert.equal(ts.encode("a b%\n\x7fé日"), "a%20b%25%0A%7Fé日");
  assert.equal(ts.decode("a%20b%25%0A%7Fé日"), "a b%\n\x7fé日");
  assert.equal(ts.decode("%2"), null);
  assert.equal(ts.decode("%2g"), null);
  assert.equal(ts.utf8("aé日😀"), 1 + 2 + 3 + 4);
});

test("the surrounding text carries byte offsets and its cause", () => {
  const s = new ts.TextSession();
  assert.equal(
    s.surroundingLine("hé", "", "llo wörld"),
    "ti_surrounding 3 3 1 hé" + "llo%20wörld",
  );
  assert.equal(s.surroundingLine("a", "bc", "d"), "ti_surrounding 3 1 1 abcd");
  // After the input method's own edit, the change is the input method's.
  const ed = new Recorder();
  batch(s, ed, ["ti_commit x", "ti_preedit -1 -1", "ti_done 1"]);
  assert.match(s.surroundingLine("ax", "", ""), /^ti_surrounding 2 2 0 /);
  assert.match(s.surroundingLine("ax", "", ""), /^ti_surrounding 2 2 1 /);
});

test("long text is trimmed around the selection without splitting a pair", () => {
  const s = new ts.TextSession();
  const line = s.surroundingLine("😀".repeat(200), "", "%".repeat(300));
  assert.ok(ts.utf8(line) <= ts.MAX_LINE);
  const text = ts.decode(line.split(" ")[4]);
  assert.ok(!/^[\udc00-\udfff]/.test(text));
  const [cursor, anchor] = line.split(" ").slice(1, 3).map(Number);
  assert.equal(cursor, anchor);
  assert.ok(text.slice(0, cursor / 2).match(/^(😀)*$/));
  // A selection too long to send is left out.
  assert.equal(
    s.surroundingLine("", "%".repeat(400), ""),
    "ti_surrounding 0 0 1 ",
  );
});

test("commits, deletes and previews apply in the protocol's order", () => {
  const s = new ts.TextSession();
  s.surroundingLine("ni hé", "", "!");
  const ed = new Recorder();
  batch(s, ed, ["ti_preedit 1 1 n", "ti_done 1"]);
  assert.deepEqual(ed.calls, [["preview", "n"]]);
  ed.calls = [];
  batch(s, ed, ["ti_preedit 2 2 ni", "ti_done 1"]);
  assert.deepEqual(ed.calls, [["preview", "ni"]]);
  ed.calls = [];
  // é is two bytes and one unit.
  batch(s, ed, [
    "ti_delete 3 1",
    "ti_commit 你",
    "ti_commit 好%20",
    "ti_preedit -1 -1",
    "ti_done 1",
  ]);
  assert.deepEqual(ed.calls, [
    ["clear"],
    ["before", 2],
    ["after", 1],
    ["insert", "你好 "],
  ]);
  ed.calls = [];
  // Results for no editor are dropped.
  batch(s, null, ["ti_commit lost", "ti_preedit -1 -1", "ti_done 1"]);
  batch(s, ed, ["ti_preedit -1 -1", "ti_done 1"]);
  assert.deepEqual(ed.calls, []);
  assert.ok(!s.onLine("key 1 30 1", ed));
});

test("deletes beyond the sent text count one unit per byte", () => {
  const s = new ts.TextSession();
  s.surroundingLine("😀", "", "");
  const ed = new Recorder();
  batch(s, ed, ["ti_delete 6 0", "ti_preedit -1 -1", "ti_done 1"]);
  assert.deepEqual(ed.calls, [["before", 4]]);
});
