import unittest

from lib.linkproxy import HandshakeFault


class HandshakeFaultTest(unittest.TestCase):
    def test_only_one_matching_get_fails_and_other_devices_are_untouched(self):
        fault = HandshakeFault()
        control = {"fail_handshake_once_for": "new-device"}
        path = "/v1/e2ee/conversations/self/handshake?after=0&device_id=new-device"
        self.assertFalse(fault.take("GET", path.replace("new-device", "old-device"), control))
        self.assertFalse(fault.take("POST", path, control))
        self.assertTrue(fault.take("GET", path, control))
        self.assertFalse(fault.take("GET", path, control))

    def test_disabled_fault_does_not_consume_its_one_failure(self):
        fault = HandshakeFault()
        path = "/v1/e2ee/conversations/self/handshake?device_id=new-device"
        self.assertFalse(fault.take("GET", path, {}))
        self.assertTrue(fault.take("GET", path, {"fail_handshake_once_for": "new-device"}))
