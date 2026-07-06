package com.openwire.app

import android.content.Context
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  private external fun initNdkContext(context: Context)

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    try {
      initNdkContext(applicationContext)
    } catch (e: UnsatisfiedLinkError) {
      android.util.Log.e("OpenWire", "JNI initNdkContext not found", e)
    } catch (e: Exception) {
      android.util.Log.e("OpenWire", "Failed to init NDK context", e)
    }
  }
}