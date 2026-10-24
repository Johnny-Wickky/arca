// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * Локальный приемник будильника для периодических ненавязчивых напоминаний о поддержке
 * студентов-разработчиков Pharos.
 *
 * <p>Безопасность: компонент строго не экспортирован ({@code exported="false"}),
 * не принимает внешних данных, не обращается к сети и не будит процессор ночью.
 */
public final class RemindReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context context, Intent intent) {
        if (context == null) {
            return;
        }
        Reminders.postNotification(context);
    }
}
