package dev.tinyclient.app;

import android.content.ClipData;
import android.content.Intent;
import android.net.Uri;

import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;

import java.util.ArrayList;
import java.util.List;

import static org.junit.Assert.assertEquals;

@RunWith(RobolectricTestRunner.class)
public class ShareItemsTest {
    @Test
    public void mergesClipDataAndStreamsInOrderAndIgnoresFileUris() {
        Uri photo = Uri.parse("content://gallery/photo/2");
        Uri document = Uri.parse("content://documents/9");
        Intent intent = new Intent(Intent.ACTION_SEND_MULTIPLE);
        intent.setClipData(ClipData.newRawUri("Photo", photo));
        intent.putParcelableArrayListExtra(Intent.EXTRA_STREAM,
                new ArrayList<>(List.of(photo, Uri.parse("file:///data/local/readme"), document)));
        assertEquals(List.of(photo, document), new ArrayList<>(ShareItems.uris(intent)));
    }

    @Test
    public void fileNamesCannotCreateDirectories() {
        assertEquals("folder_file.pdf", ShareItems.cleanName("folder/file.pdf"));
        assertEquals("attachment", ShareItems.cleanName(".."));
    }
}
