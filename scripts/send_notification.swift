#!/usr/bin/env swift
import Foundation

let notificationName = CommandLine.arguments[1]
let center = DistributedNotificationCenter.default()

// CRITICAL: postNotificationName with options for cross-process delivery
center.postNotificationName(
    NSNotification.Name(notificationName),
    object: nil,
    userInfo: nil,
    deliverImmediately: true
)

print("Sent notification: \(notificationName) with deliverImmediately=true")
