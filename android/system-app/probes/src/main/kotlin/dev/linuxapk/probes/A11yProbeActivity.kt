// SPDX-License-Identifier: AGPL-3.0-or-later
package dev.linuxapk.probes

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.TextView

/**
 * The accessibility fixture (host/bridge/src/a11y/, tests/e2e/a11y): a button that counts its
 * clicks into a label, a checkbox, a text field and a list, as plain framework widgets. Logs (tag
 * `LinuxApkProbe`) each click and check.
 *
 * `am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.LAUNCH --es package
 * dev.linuxapk.probes --es component dev.linuxapk.probes/.A11yProbeActivity`
 */
class A11yProbeActivity : Activity() {
  private var count = 0

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val counter = TextView(this).apply { text = "Count 0" }
    val button =
      Button(this).apply {
        text = "Increment"
        isAllCaps = false
        setOnClickListener {
          count++
          counter.text = "Count $count"
          Log.i(TAG, "a11y probe: count $count")
        }
      }
    val check =
      CheckBox(this).apply {
        text = "Remember me"
        setOnCheckedChangeListener { _, on -> Log.i(TAG, "a11y probe: checked $on") }
      }
    val name = EditText(this).apply { hint = "Name" }
    val list =
      ListView(this).apply {
        adapter =
          ArrayAdapter(
            this@A11yProbeActivity,
            android.R.layout.simple_list_item_1,
            listOf("Apple", "Banana", "Cherry"),
          )
      }
    setContentView(
      LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(32, 32, 32, 32)
        addView(button)
        addView(counter)
        addView(check)
        addView(name)
        addView(list)
      },
    )
  }

  companion object {
    const val TAG = "LinuxApkProbe"
  }
}
