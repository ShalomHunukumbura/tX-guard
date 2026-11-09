from django.db import transaction

from .tasks import ship_order


def place_order(cart):
    with transaction.atomic():
        order = cart.to_order()
        transaction.on_commit(lambda: ship_order.delay(order.id))
    return order
